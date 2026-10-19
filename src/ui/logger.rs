//! A compact, `consola`-flavoured log format.
//!
//! The default `tracing` output is built for servers: a timestamp, a level, a
//! target, then key-value pairs. For a tool a person watches live, that is a lot
//! of noise around a short message. This formatter puts a glyph and the message
//! first, and dims everything supporting.
//!
//! ```text
//! 12:04:31 ✔ delivered to Tech News  route=mirror via=forward took=812ms
//! ```

use std::fmt;
use std::io;

use tracing::field::{Field, Visit};
use tracing::{Event, Level as TracingLevel, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

use super::theme::{self, Level};

/// Log filtering applied when `RUST_LOG` says nothing.
///
/// The `MTProto` stack logs at debug level constantly; surfacing that by default
/// would bury the messages a user actually cares about.
const DEFAULT_FILTER: &str = "tgfwd=info,warn";

/// Install the global logger.
///
/// `verbosity` counts `-v` flags: 0 is the default, 1 adds this crate's debug
/// output, 2 or more turns on the underlying Telegram library as well.
/// An explicit `RUST_LOG` always wins.
pub fn init(verbosity: u8) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Some dependencies still log through the `log` crate, so bridge it into
    // `tracing`, otherwise their warnings would be invisible.
    tracing_log::LogTracer::init()?;

    let filter = match std::env::var("RUST_LOG") {
        Ok(value) if !value.is_empty() => EnvFilter::new(value),
        _ => EnvFilter::new(match verbosity {
            0 => DEFAULT_FILTER.to_owned(),
            1 => "tgfwd=debug,warn".to_owned(),
            _ => "tgfwd=trace,grammers_client=debug,grammers_mtsender=debug".to_owned(),
        }),
    };

    let subscriber = tracing_subscriber::fmt()
        .event_format(ConsolaFormat)
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}

/// The event formatter described in the module docs.
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsolaFormat;

impl<S, N> FormatEvent<S, N> for ConsolaFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let level = match *event.metadata().level() {
            TracingLevel::ERROR => Level::Error,
            TracingLevel::WARN => Level::Warn,
            TracingLevel::INFO => Level::Info,
            _ => Level::Debug,
        };

        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);

        // An `ok` field marks a success, which reads better as a green tick than
        // as the neutral info marker. The glyph follows the message: a line that
        // says something worked should not open with the same symbol as one
        // reporting that a chat list is being refreshed.
        let style = if visitor.success && level == Level::Info {
            Level::Success
        } else {
            level
        };

        let now = chrono::Local::now().format("%H:%M:%S");
        write!(writer, "{} ", theme::dim(&now.to_string()))?;
        write!(writer, "{} ", style.paint(style.glyph()))?;
        write!(writer, "{}", style.paint(&visitor.message))?;

        if !visitor.fields.is_empty() {
            write!(writer, "  {}", theme::dim(&visitor.fields.join(" ")))?;
        }

        writeln!(writer)
    }
}

/// Collects an event's message and its remaining fields.
#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: Vec<String>,
    /// Set by a field literally named `ok`, used to colour successes green.
    success: bool,
}

impl Visit for EventVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "ok" {
            self.success = value;
            return;
        }
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            // `Debug` rendering of a string literal keeps its quotes; strip them
            // so messages do not read as `"like this"`.
            rendered.trim_matches('"').clone_into(&mut self.message);
        } else {
            self.fields
                .push(format!("{}={}", field.name(), rendered.trim_matches('"')));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Render events through the real formatter and return what was written.
    fn render(emit: impl FnOnce()) -> String {
        #[derive(Clone)]
        struct Buffer(std::sync::Arc<Mutex<Vec<u8>>>);

        impl io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("buffer").extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Buffer(std::sync::Arc::new(Mutex::new(Vec::new())));
        let subscriber = tracing_subscriber::fmt()
            .event_format(ConsolaFormat)
            .with_writer(sink.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, emit);
        let written = sink.0.lock().expect("buffer").clone();
        String::from_utf8(written).expect("utf-8")
    }

    #[test]
    fn the_message_leads_and_the_rest_trails_as_fields() {
        let line = render(|| tracing::info!(route = "mirror", "refreshing chat list"));

        assert!(line.contains("refreshing chat list"), "{line}");
        assert!(line.contains("route=mirror"), "{line}");
        assert!(
            line.find("refreshing").unwrap() < line.find("route=").unwrap(),
            "the message should come before its fields: {line}"
        );
    }

    #[test]
    fn a_success_is_marked_differently_from_ordinary_news() {
        // The whole point of the `ok` marker: at a glance, "it worked" must not
        // look like "here is some information".
        let success =
            render(|| tracing::info!(ok = true, via = "forward", "delivered to Tech News"));
        let plain = render(|| tracing::info!("refreshing chat list"));

        assert!(success.contains(Level::Success.glyph()), "{success}");
        assert!(plain.contains(Level::Info.glyph()), "{plain}");
        assert!(
            !success.contains("ok=true"),
            "the marker drives the styling, it is not itself news: {success}"
        );
        assert!(success.contains("via=forward"), "{success}");
    }

    #[test]
    fn a_quoted_debug_message_loses_its_quotes() {
        // `tracing` records a bare string literal through `record_debug`, which
        // renders it with the quotes still attached.
        let line = render(|| tracing::warn!("could not persist the session"));
        assert!(line.contains("could not persist the session"), "{line}");
        assert!(!line.contains('"'), "{line}");
    }
}
