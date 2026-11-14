// Clock times for display and for typing ("1:02:03").

// 3723.4 -> "1:02:03"
pub fn clock(secs: f64) -> String {
    let t = secs.max(0.0).round() as u64;
    format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

// "1:02:03", "2:03" or plain seconds.
pub fn parse_time(text: &str) -> Result<f64, String> {
    let parts: Result<Vec<f64>, _> = text.trim().split(':').map(|p| p.trim().parse::<f64>()).collect();
    match parts {
        Ok(p) if p.len() <= 3 && p.iter().all(|v| v.is_finite() && *v >= 0.0) => Ok(p.iter().fold(0.0, |sum, v| sum * 60.0 + v)),
        _ => Err("Enter a time like 1:02:03, 2:03 or 95 (seconds).".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_hours_minutes_seconds() {
        assert_eq!(clock(0.0), "0:00:00");
        assert_eq!(clock(3723.4), "1:02:03");
        assert_eq!(clock(-5.0), "0:00:00");
    }

    #[test]
    fn parse_time_reads_clock_and_seconds() {
        assert_eq!(parse_time("1:02:03"), Ok(3723.0));
        assert_eq!(parse_time(" 2:03 "), Ok(123.0));
        assert_eq!(parse_time("95"), Ok(95.0));
        assert_eq!(parse_time("1:30.5"), Ok(90.5));
        for bad in ["", "abc", "-5", "1:2:3:4", "NaN", "1::2"] {
            assert!(parse_time(bad).is_err(), "{}", bad);
        }
    }
}
