//! Colours and number formatting for the terminal output.

use std::io::IsTerminal;

#[derive(Clone, Copy, Debug)]
pub struct Ui {
    pub color: bool,
}

impl Ui {
    /// Colours when the stream is a terminal, unless `NO_COLOR` is set.
    pub fn for_stream(is_tty: bool) -> Ui {
        let allowed = std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").map_or(true, |t| t != "dumb");
        Ui {
            color: is_tty && allowed,
        }
    }

    pub fn stderr() -> Ui {
        Ui::for_stream(std::io::stderr().is_terminal())
    }

    pub fn stdout() -> Ui {
        Ui::for_stream(std::io::stdout().is_terminal())
    }

    pub fn plain() -> Ui {
        Ui { color: false }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
}

/// `850ms`, `3.2s`, `1m 12s`, `1h 02m`.
pub fn duration(secs: f64) -> String {
    if secs < 1.0 {
        format!("{}ms", (secs * 1000.0).round() as u64)
    } else if secs < 10.0 {
        format!("{secs:.1}s")
    } else if secs < 60.0 {
        format!("{}s", secs.round() as u64)
    } else if secs < 3600.0 {
        let s = secs.round() as u64;
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        let m = (secs / 60.0).round() as u64;
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(0.25), "250ms");
        assert_eq!(duration(3.24), "3.2s");
        assert_eq!(duration(42.0), "42s");
        assert_eq!(duration(72.0), "1m 12s");
        assert_eq!(duration(3720.0), "1h 02m");
    }

    #[test]
    fn plain_output_has_no_escape_codes() {
        assert_eq!(Ui::plain().red("x"), "x");
        assert!(Ui { color: true }.red("x").contains("\x1b[31m"));
    }
}
