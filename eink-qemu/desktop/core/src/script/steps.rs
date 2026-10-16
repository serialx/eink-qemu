//! The step language of scripted runs: `kind` or `kind:argument`, one step
//! per command-line argument or per line of a script file. The argument is
//! everything after the first colon, so a regex may contain colons and, in a
//! script file, spaces without quoting.

use regex::Regex;
use std::path::PathBuf;

/// Every step, with its argument form and meaning. Shown by `eink-qemu
/// steps` and in error messages.
pub const STEP_HELP: &str = "\
Waiting
  ready[:SECONDS]            boot finished: Home drawn and the firmware reporting
                             (CrossPoint), or the screen quiet for 5 s (others)
  wait-log:REGEX             a log line matching REGEX, after the last action
  wait-screen[:SECONDS]      the screen changed after the last action and settled
  wait:SECONDS               let the guest run (guest time)
  timeout:SECONDS            time limit of later waits (default 60 s)
Input (each is an action: later waits look only at what follows it)
  press:KEY[:MS]             press and release a key (hold 150 ms of guest time)
                             X3, X4: back confirm left right up down power
                             X4 Pro: up down power home    Picco: up down power
  touch:X,Y[:MS]             tap the touchscreen (X4 Pro, Picco) at panel pixels,
                             or percentages: touch:50%,90%
  swipe:DIR[:MS]             swipe left, right, up or down across the middle
  swipe:X1,Y1,X2,Y2[:MS]     swipe between two points (default 300 ms)
  reset                      reset the device (the SD card and flash are kept)
Capture
  shot:NAME                  the settled screen as shots/NAME.png
  heap[:NAME]                walk every heap; heaps/NAME.json with blocks
  trace-start:NAME           mark the start of an allocation window (--trace)
  trace-stop:NAME            allocations, frees and bytes kept since trace-start
  mark:NAME                  a labelled record, for reports and comparisons
Checks
  expect-log:REGEX           some log line so far matches REGEX
  expect-no-log:REGEX        no log line so far matches REGEX
  expect-shot:NAME=PNG[@X,Y,W,H...]
                             shot NAME equals PNG, outside the masked rectangles
Structure
  repeat:N ... end           run the steps between N times
  hmp:COMMAND                a QEMU monitor command; its output is recorded
";

/// A step and the text it was written as.
#[derive(Debug, Clone)]
pub struct Step {
    pub kind: Kind,
    pub text: String,
}

/// A point on the panel: pixels, or a fraction of its width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Coord {
    Px(f64),
    Frac(f64),
}

impl Coord {
    pub fn resolve(self, size: u32) -> f64 {
        match self {
            Coord::Px(v) => v,
            Coord::Frac(f) => f * (size.saturating_sub(1)) as f64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Ready { timeout: Option<f64> },
    Wait { seconds: f64 },
    WaitLog { pattern: Regex },
    WaitScreen { timeout: Option<f64> },
    Timeout { seconds: f64 },
    Press { key: String, ms: u32 },
    Touch { x: Coord, y: Coord, ms: u32 },
    Swipe { from: (Coord, Coord), to: (Coord, Coord), ms: u32 },
    Reset,
    Shot { name: String },
    Heap { name: Option<String> },
    TraceStart { name: String },
    TraceStop { name: String },
    Mark { name: String },
    ExpectLog { pattern: Regex },
    ExpectNoLog { pattern: Regex },
    ExpectShot { name: String, reference: PathBuf, masks: Vec<Rect> },
    Repeat { count: u32, body: Vec<Step> },
    Hmp { command: String },
}

impl Kind {
    /// The step's kind as written.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Ready { .. } => "ready",
            Kind::Wait { .. } => "wait",
            Kind::WaitLog { .. } => "wait-log",
            Kind::WaitScreen { .. } => "wait-screen",
            Kind::Timeout { .. } => "timeout",
            Kind::Press { .. } => "press",
            Kind::Touch { .. } => "touch",
            Kind::Swipe { .. } => "swipe",
            Kind::Reset => "reset",
            Kind::Shot { .. } => "shot",
            Kind::Heap { .. } => "heap",
            Kind::TraceStart { .. } => "trace-start",
            Kind::TraceStop { .. } => "trace-stop",
            Kind::Mark { .. } => "mark",
            Kind::ExpectLog { .. } => "expect-log",
            Kind::ExpectNoLog { .. } => "expect-no-log",
            Kind::ExpectShot { .. } => "expect-shot",
            Kind::Repeat { .. } => "repeat",
            Kind::Hmp { .. } => "hmp",
        }
    }

    /// Input steps: waits after one look only at what follows it.
    pub fn is_action(&self) -> bool {
        matches!(self, Kind::Press { .. } | Kind::Touch { .. } | Kind::Swipe { .. } | Kind::Reset)
    }

    /// The name a step gives its output, if any.
    pub fn label(&self) -> Option<&str> {
        match self {
            Kind::Shot { name } | Kind::TraceStart { name } | Kind::TraceStop { name } | Kind::Mark { name }
            | Kind::ExpectShot { name, .. } => Some(name),
            Kind::Heap { name } => name.as_deref(),
            _ => None,
        }
    }
}

const KINDS: &[&str] = &[
    "ready", "wait", "wait-log", "wait-screen", "timeout", "press", "touch", "swipe", "reset", "shot", "heap",
    "trace-start", "trace-stop", "mark", "expect-log", "expect-no-log", "expect-shot", "repeat", "end", "hmp",
];

/// The lines of a script file that hold steps: blank lines and lines
/// starting with # are skipped; surrounding whitespace is trimmed.
pub fn script_lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect()
}

/// Parse steps, matching `repeat:N` with `end`.
pub fn parse(tokens: &[String]) -> Result<Vec<Step>, String> {
    let mut stack: Vec<(u32, String, Vec<Step>)> = Vec::new();
    let mut top: Vec<Step> = Vec::new();
    for token in tokens {
        let token = token.trim();
        let (kind, arg) = match token.split_once(':') {
            Some((k, a)) => (k, Some(a)),
            None => (token, None),
        };
        if kind == "end" {
            if arg.is_some() {
                return Err(format!("`{token}`: end takes no argument"));
            }
            let Some((count, text, body)) = stack.pop() else {
                return Err("`end` without a `repeat:N` before it".into());
            };
            let step = Step { kind: Kind::Repeat { count, body }, text };
            match stack.last_mut() {
                Some((_, _, outer)) => outer.push(step),
                None => top.push(step),
            }
            continue;
        }
        if kind == "repeat" {
            let count = number(token, arg, "a count")? as u32;
            if count == 0 {
                return Err(format!("`{token}`: the count must be at least 1"));
            }
            stack.push((count, token.to_string(), Vec::new()));
            continue;
        }
        let step = Step { kind: parse_one(token, kind, arg)?, text: token.to_string() };
        match stack.last_mut() {
            Some((_, _, body)) => body.push(step),
            None => top.push(step),
        }
    }
    if let Some((_, text, _)) = stack.last() {
        return Err(format!("`{text}` has no matching `end`"));
    }
    Ok(top)
}

fn usage(kind: &str) -> String {
    let lines: Vec<&str> = STEP_HELP
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            l.strip_prefix(kind).is_some_and(|rest| rest.starts_with([':', '[', ' ']))
        })
        .collect();
    match lines.first() {
        Some(line) => format!("usage: {}", line.trim()),
        None => String::new(),
    }
}

fn number(token: &str, arg: Option<&str>, what: &str) -> Result<f64, String> {
    let kind = token.split(':').next().unwrap_or(token);
    let text = arg.ok_or_else(|| format!("`{token}` needs {what}; {}", usage(kind)))?;
    let value: f64 = text.trim().parse().map_err(|_| format!("`{token}`: {text:?} is not a number; {}", usage(kind)))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("`{token}`: {what} must be zero or more"));
    }
    Ok(value)
}

fn optional_number(token: &str, arg: Option<&str>, what: &str) -> Result<Option<f64>, String> {
    match arg {
        None | Some("") => Ok(None),
        Some(_) => number(token, arg, what).map(Some),
    }
}

fn pattern(token: &str, kind: &str, arg: Option<&str>) -> Result<Regex, String> {
    let text = arg.filter(|a| !a.is_empty()).ok_or_else(|| format!("`{token}` needs a regex; {}", usage(kind)))?;
    Regex::new(text).map_err(|e| format!("`{token}`: invalid regex: {e}"))
}

fn name(token: &str, kind: &str, arg: Option<&str>) -> Result<String, String> {
    let text = arg.filter(|a| !a.is_empty()).ok_or_else(|| format!("`{token}` needs a name; {}", usage(kind)))?;
    if !text.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) || text.starts_with('.') {
        return Err(format!("`{token}`: names use letters, digits, '.', '_' and '-' (they become file names)"));
    }
    Ok(text.to_string())
}

fn coord(token: &str, text: &str) -> Result<Coord, String> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent.parse().map_err(|_| format!("`{token}`: {text:?} is not a percentage"))?;
        if !(0.0..=100.0).contains(&value) {
            return Err(format!("`{token}`: {text} is outside 0% to 100%"));
        }
        return Ok(Coord::Frac(value / 100.0));
    }
    let value: f64 = text.parse().map_err(|_| format!("`{token}`: {text:?} is not a pixel coordinate or a percentage"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("`{token}`: coordinates are zero or more"));
    }
    Ok(Coord::Px(value))
}

fn hold(token: &str, text: Option<&str>, default: u32) -> Result<u32, String> {
    match text {
        None => Ok(default),
        Some(ms) => ms.trim().parse::<u32>().map_err(|_| format!("`{token}`: {ms:?} is not a number of milliseconds"))
            .and_then(|ms| if ms == 0 || ms > 60_000 { Err(format!("`{token}`: hold 1 to 60000 ms")) } else { Ok(ms) }),
    }
}

fn parse_one(token: &str, kind: &str, arg: Option<&str>) -> Result<Kind, String> {
    Ok(match kind {
        "ready" => Kind::Ready { timeout: optional_number(token, arg, "a time limit in seconds")? },
        "wait" => Kind::Wait { seconds: number(token, arg, "a number of seconds")? },
        "wait-log" => Kind::WaitLog { pattern: pattern(token, kind, arg)? },
        "wait-screen" => Kind::WaitScreen { timeout: optional_number(token, arg, "a time limit in seconds")? },
        "timeout" => {
            let seconds = number(token, arg, "a number of seconds")?;
            if seconds == 0.0 {
                return Err(format!("`{token}`: the time limit must be more than 0"));
            }
            Kind::Timeout { seconds }
        }
        "press" => {
            let arg = arg.filter(|a| !a.is_empty()).ok_or_else(|| format!("`{token}` needs a key; {}", usage(kind)))?;
            let (key, ms) = match arg.split_once(':') {
                Some((k, ms)) => (k, Some(ms)),
                None => (arg, None),
            };
            Kind::Press { key: key.trim().to_ascii_lowercase(), ms: hold(token, ms, 150)? }
        }
        "touch" => {
            let arg = arg.ok_or_else(|| format!("`{token}` needs X,Y; {}", usage(kind)))?;
            let (point, ms) = match arg.split_once(':') {
                Some((p, ms)) => (p, Some(ms)),
                None => (arg, None),
            };
            let parts: Vec<&str> = point.split(',').collect();
            let [x, y] = parts[..] else { return Err(format!("`{token}` needs X,Y; {}", usage(kind))) };
            Kind::Touch { x: coord(token, x)?, y: coord(token, y)?, ms: hold(token, ms, 100)? }
        }
        "swipe" => {
            let arg = arg.ok_or_else(|| format!("`{token}` needs a direction or X1,Y1,X2,Y2; {}", usage(kind)))?;
            let (path, ms) = match arg.split_once(':') {
                Some((p, ms)) => (p, Some(ms)),
                None => (arg, None),
            };
            let ms = hold(token, ms, 300)?;
            let f = Coord::Frac;
            let (from, to) = match path.trim() {
                "left" => ((f(0.8), f(0.5)), (f(0.2), f(0.5))),
                "right" => ((f(0.2), f(0.5)), (f(0.8), f(0.5))),
                "up" => ((f(0.5), f(0.8)), (f(0.5), f(0.2))),
                "down" => ((f(0.5), f(0.2)), (f(0.5), f(0.8))),
                points => {
                    let parts: Vec<&str> = points.split(',').collect();
                    let [x1, y1, x2, y2] = parts[..] else {
                        return Err(format!("`{token}` needs left, right, up, down or X1,Y1,X2,Y2"));
                    };
                    ((coord(token, x1)?, coord(token, y1)?), (coord(token, x2)?, coord(token, y2)?))
                }
            };
            Kind::Swipe { from, to, ms }
        }
        "reset" => {
            if arg.is_some() {
                return Err(format!("`{token}`: reset takes no argument"));
            }
            Kind::Reset
        }
        "shot" => Kind::Shot { name: name(token, kind, arg)? },
        "heap" => Kind::Heap { name: match arg { None | Some("") => None, Some(_) => Some(name(token, kind, arg)?) } },
        "trace-start" => Kind::TraceStart { name: name(token, kind, arg)? },
        "trace-stop" => Kind::TraceStop { name: name(token, kind, arg)? },
        "mark" => Kind::Mark { name: name(token, kind, arg)? },
        "expect-log" => Kind::ExpectLog { pattern: pattern(token, kind, arg)? },
        "expect-no-log" => Kind::ExpectNoLog { pattern: pattern(token, kind, arg)? },
        "expect-shot" => {
            let arg = arg.ok_or_else(|| format!("`{token}` needs NAME=PNG; {}", usage(kind)))?;
            let (shot, rest) = arg.split_once('=').ok_or_else(|| format!("`{token}` needs NAME=PNG; {}", usage(kind)))?;
            let shot = name(token, kind, Some(shot))?;
            let mut parts = rest.split('@');
            let reference = PathBuf::from(parts.next().unwrap_or_default());
            if reference.as_os_str().is_empty() {
                return Err(format!("`{token}` needs the reference PNG after '='"));
            }
            let mut masks = Vec::new();
            for mask in parts {
                let n: Vec<u32> = mask.split(',').map(|v| v.trim().parse::<u32>()).collect::<Result<_, _>>()
                    .map_err(|_| format!("`{token}`: mask {mask:?} is not X,Y,W,H in pixels"))?;
                let [x, y, w, h] = n[..] else { return Err(format!("`{token}`: mask {mask:?} is not X,Y,W,H")) };
                masks.push(Rect { x, y, w, h });
            }
            Kind::ExpectShot { name: shot, reference, masks }
        }
        "hmp" => Kind::Hmp {
            command: arg.filter(|a| !a.is_empty()).ok_or_else(|| format!("`{token}` needs a monitor command"))?.to_string(),
        },
        other => {
            let near = KINDS.iter().filter(|k| close(other, k)).copied().collect::<Vec<_>>();
            let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.join(" or ")) };
            let what = if token.contains('/') || token.contains('.') && !token.contains(':') {
                " (options such as --sd go before the firmware; steps come after it)"
            } else {
                ""
            };
            return Err(format!("unknown step `{token}`{what}.{hint} `eink-qemu steps` lists every step."));
        }
    })
}

/// Edit distance of at most two, or one name inside the other.
fn close(a: &str, b: &str) -> bool {
    if a.len() >= 3 && (b.contains(a) || a.contains(b)) {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut row = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (prev[j] + 1).min(row[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = row;
    }
    prev[b.len()] <= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(tokens: &[&str]) -> Result<Vec<Step>, String> {
        parse(&tokens.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_the_design_examples() {
        let parsed = steps(&[
            "ready", "wait-log:Rendered page in", "press:confirm", "wait-log:Loaded ePub", "heap:cold-open", "shot:p1",
            "repeat:12", "press:down", "wait-log:Page render", "end", "heap:after-turns", "shot:p13",
        ])
        .unwrap();
        assert_eq!(parsed.len(), 9);
        let Kind::Repeat { count, body } = &parsed[6].kind else { panic!() };
        assert_eq!((*count, body.len()), (12, 2));
    }

    #[test]
    fn regexes_keep_their_colons() {
        let parsed = steps(&["wait-log:a:b c", "expect-no-log:Guru Meditation"]).unwrap();
        let Kind::WaitLog { pattern } = &parsed[0].kind else { panic!() };
        assert_eq!(pattern.as_str(), "a:b c");
    }

    #[test]
    fn input_steps() {
        let parsed = steps(&["press:Down:500", "touch:50%,90%", "swipe:left", "swipe:10,20,30,40:200"]).unwrap();
        assert!(matches!(&parsed[0].kind, Kind::Press { key, ms: 500 } if key == "down"));
        assert!(matches!(parsed[1].kind, Kind::Touch { x: Coord::Frac(_), y: Coord::Frac(_), ms: 100 }));
        assert!(matches!(parsed[2].kind, Kind::Swipe { ms: 300, .. }));
        assert!(matches!(parsed[3].kind, Kind::Swipe { from: (Coord::Px(_), Coord::Px(_)), ms: 200, .. }));
    }

    #[test]
    fn helpful_errors() {
        assert!(steps(&["wait-logs:x"]).unwrap_err().contains("wait-log"));
        assert!(steps(&["repeat:2", "press:down"]).unwrap_err().contains("no matching `end`"));
        assert!(steps(&["end"]).unwrap_err().contains("without"));
        assert!(steps(&["shot:a/b"]).unwrap_err().contains("names use"));
        assert!(steps(&["wait"]).unwrap_err().contains("usage: wait:SECONDS"));
        assert!(steps(&["book.epub"]).unwrap_err().contains("go before the firmware"));
        let parsed = steps(&["expect-shot:p1=ref/p1.png@0,0,10,20@5,5,1,1"]).unwrap();
        let Kind::ExpectShot { masks, .. } = &parsed[0].kind else { panic!() };
        assert_eq!(masks.len(), 2);
    }

    #[test]
    fn script_files_skip_comments() {
        assert_eq!(script_lines("# boot\nready\n\n  wait-log:Page render  \n"), ["ready", "wait-log:Page render"]);
    }
}
