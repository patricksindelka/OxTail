//! Deterministic synthetic log line generator shared by `gen-log` and `append-log`.

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Plain,
    Nginx,
    Apache,
    Jsonl,
    Logfmt,
    Log4j,
    Syslog,
    Syslog5424,
    Csv,
    Iis,
}

impl Format {
    pub const NAMES: &'static str =
        "plain|nginx|apache|jsonl|logfmt|log4j|syslog|syslog5424|csv|iis";

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "plain" => Self::Plain,
            "nginx" => Self::Nginx,
            "apache" => Self::Apache,
            "jsonl" => Self::Jsonl,
            "logfmt" => Self::Logfmt,
            "log4j" => Self::Log4j,
            "syslog" => Self::Syslog,
            "syslog5424" => Self::Syslog5424,
            "csv" => Self::Csv,
            "iis" => Self::Iis,
            _ => bail!("unknown format `{s}` (expected {})", Self::NAMES),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Latin1,
}

impl Encoding {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "utf8" | "utf-8" => Self::Utf8,
            "utf16le" | "utf-16le" => Self::Utf16Le,
            "latin1" | "iso-8859-1" => Self::Latin1,
            _ => bail!("unknown encoding `{s}` (expected utf8|utf16le|latin1)"),
        })
    }

    /// Bytes to write at the start of a file (BOM for UTF-16LE).
    pub fn bom(self) -> &'static [u8] {
        match self {
            Self::Utf16Le => &[0xFF, 0xFE],
            _ => &[],
        }
    }

    /// Transcode a UTF-8 chunk (must end on a char boundary) into `out`.
    pub fn encode(self, src: &[u8], out: &mut Vec<u8>) {
        out.clear();
        match self {
            Self::Utf8 => out.extend_from_slice(src),
            Self::Latin1 => {
                if src.is_ascii() {
                    out.extend_from_slice(src);
                } else {
                    for c in String::from_utf8_lossy(src).chars() {
                        out.push(if (c as u32) < 256 {
                            c as u32 as u8
                        } else {
                            b'?'
                        });
                    }
                }
            }
            Self::Utf16Le => {
                out.reserve(src.len() * 2);
                if src.is_ascii() {
                    for &b in src {
                        out.push(b);
                        out.push(0);
                    }
                } else {
                    let mut tmp = [0u16; 2];
                    for c in String::from_utf8_lossy(src).chars() {
                        for u in c.encode_utf16(&mut tmp) {
                            out.extend_from_slice(&u.to_le_bytes());
                        }
                    }
                }
            }
        }
    }
}

/// Parse sizes such as `500M`, `10G`, `64k`, `1GiB`, `123` (bytes). 1024-based.
pub fn parse_size(s: &str) -> Result<u64> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let (num, suf) = t.split_at(split);
    if num.is_empty() {
        bail!("invalid size `{s}`");
    }
    let n: u64 = num.parse()?;
    let mult: u64 = match suf.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => bail!("invalid size suffix in `{s}`"),
    };
    n.checked_mul(mult)
        .ok_or_else(|| anyhow::anyhow!("size `{s}` overflows"))
}

/// Parse an RFC 3339 timestamp into microseconds since the Unix epoch.
pub fn parse_rfc3339(s: &str) -> Result<i64> {
    let b = s.as_bytes();
    let num = |a: usize, n: usize| -> Result<i64> {
        let sl = b
            .get(a..a + n)
            .filter(|x| x.iter().all(u8::is_ascii_digit))
            .ok_or_else(|| anyhow::anyhow!("invalid RFC3339 timestamp `{s}`"))?;
        Ok(sl
            .iter()
            .fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0')))
    };
    let sep_ok = b.get(4) == Some(&b'-')
        && b.get(7) == Some(&b'-')
        && matches!(b.get(10), Some(b'T' | b't' | b' '))
        && b.get(13) == Some(&b':')
        && b.get(16) == Some(&b':');
    if !sep_ok {
        bail!("invalid RFC3339 timestamp `{s}`");
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        bail!("out-of-range field in `{s}`");
    }
    let mut i = 19;
    let mut micros = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let mut scale = 100_000;
        while let Some(c) = b.get(i).filter(|c| c.is_ascii_digit()) {
            micros += i64::from(c - b'0') * scale;
            scale /= 10;
            i += 1;
        }
    }
    let offset = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if b.len() == i + 6 && b[i + 3] == b':' => {
            let v = num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60;
            if *sign == b'+' { v } else { -v }
        }
        _ => bail!("missing or invalid time zone in `{s}`"),
    };
    let days = days_from_civil(y, mo, d);
    Ok(((days * 86_400 + h * 3600 + mi * 60 + sec - offset) * 1_000_000) + micros)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn push_u(buf: &mut Vec<u8>, mut n: u64) {
    let mut tmp = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    buf.extend_from_slice(&tmp[i..]);
}

fn push_pad(buf: &mut Vec<u8>, n: u64, width: usize) {
    let mut tmp = [b'0'; 20];
    let mut n = n;
    for i in (0..width).rev() {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    buf.extend_from_slice(&tmp[..width]);
}

const LEVELS: [&str; 6] = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL"];
const LEVELS_LOWER: [&str; 6] = ["trace", "debug", "info", "warn", "error", "fatal"];

const METHODS: [&str; 6] = ["GET", "GET", "GET", "POST", "PUT", "DELETE"];
const PATHS: [&str; 16] = [
    "/",
    "/index.html",
    "/api/v1/users",
    "/api/v1/orders",
    "/api/v1/orders/items",
    "/api/v2/search",
    "/static/app.js",
    "/static/main.css",
    "/images/logo.png",
    "/login",
    "/logout",
    "/health",
    "/metrics",
    "/products/list",
    "/cart/checkout",
    "/admin/dashboard",
];
const AGENTS: [&str; 5] = [
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:127.0) Gecko/20100101 Firefox/127.0",
    "curl/8.5.0",
    "python-requests/2.32.3",
];
const REFERERS: [&str; 4] = [
    "-",
    "https://www.example.com/",
    "https://www.example.com/products/list",
    "https://search.example.org/?q=logs",
];
const STATUSES: [u16; 12] = [200, 200, 200, 200, 200, 200, 204, 301, 304, 404, 500, 503];
const HOSTS: [&str; 4] = ["web-01", "web-02", "app-01", "db-01"];
const PROCS: [&str; 5] = ["sshd", "cron", "nginx", "systemd", "kernel"];
const CLASSES: [&str; 8] = [
    "com.acme.web.OrderController",
    "com.acme.service.PaymentService",
    "com.acme.repo.UserRepository",
    "com.acme.cache.SessionCache",
    "com.acme.job.CleanupJob",
    "org.springframework.web.servlet.DispatcherServlet",
    "com.acme.net.UpstreamClient",
    "com.acme.auth.TokenValidator",
];
const THREADS: [&str; 5] = [
    "main",
    "http-nio-8080-exec-1",
    "http-nio-8080-exec-7",
    "scheduler-2",
    "pool-3-thread-4",
];
const EXCEPTIONS: [&str; 5] = [
    "java.lang.NullPointerException",
    "java.lang.IllegalStateException: connection closed",
    "java.io.IOException: Broken pipe",
    "java.util.concurrent.TimeoutException: request timed out",
    "org.acme.ValidationException: invalid order state",
];

/// Which style the timestamp is rendered in.
#[derive(Clone, Copy)]
enum Ts {
    Plain,
    Comma,
    Rfc3339,
    Clf,
    Syslog,
    IisDate,
    IisTime,
}

pub struct Generator {
    format: Format,
    rng: fastrand::Rng,
    micros: i64,
    step_mean: f64,
    multiline_ratio: f64,
    eol: &'static [u8],
    // cached civil time for the current second
    cur_sec: i64,
    y: i64,
    mo: u32,
    d: u32,
    h: u32,
    mi: u32,
    s: u32,
}

impl Generator {
    pub fn new(
        format: Format,
        seed: u64,
        start_micros: i64,
        rate: f64,
        multiline_ratio: f64,
        crlf: bool,
    ) -> Self {
        let mut g = Self {
            format,
            rng: fastrand::Rng::with_seed(seed),
            micros: start_micros,
            step_mean: 1_000_000.0 / rate.max(0.000_001),
            multiline_ratio,
            eol: if crlf { b"\r\n" } else { b"\n" },
            cur_sec: i64::MIN,
            y: 0,
            mo: 0,
            d: 0,
            h: 0,
            mi: 0,
            s: 0,
        };
        g.refresh_civil();
        g
    }

    fn refresh_civil(&mut self) {
        let sec = self.micros.div_euclid(1_000_000);
        if sec == self.cur_sec {
            return;
        }
        self.cur_sec = sec;
        let days = sec.div_euclid(86_400);
        let rem = sec.rem_euclid(86_400);
        let (y, mo, d) = civil_from_days(days);
        self.y = y;
        self.mo = mo;
        self.d = d;
        self.h = (rem / 3600) as u32;
        self.mi = (rem % 3600 / 60) as u32;
        self.s = (rem % 60) as u32;
    }

    fn ts(&self, buf: &mut Vec<u8>, style: Ts) {
        let ms = (self.micros.rem_euclid(1_000_000) / 1000) as u64;
        let (y, mo, d) = (self.y as u64, u64::from(self.mo), u64::from(self.d));
        let (h, mi, s) = (u64::from(self.h), u64::from(self.mi), u64::from(self.s));
        match style {
            Ts::Plain | Ts::Comma | Ts::Rfc3339 => {
                push_pad(buf, y, 4);
                buf.push(b'-');
                push_pad(buf, mo, 2);
                buf.push(b'-');
                push_pad(buf, d, 2);
                buf.push(if matches!(style, Ts::Rfc3339) {
                    b'T'
                } else {
                    b' '
                });
                push_pad(buf, h, 2);
                buf.push(b':');
                push_pad(buf, mi, 2);
                buf.push(b':');
                push_pad(buf, s, 2);
                buf.push(if matches!(style, Ts::Comma) {
                    b','
                } else {
                    b'.'
                });
                push_pad(buf, ms, 3);
                if matches!(style, Ts::Rfc3339) {
                    buf.push(b'Z');
                }
            }
            Ts::Clf => {
                push_pad(buf, d, 2);
                buf.push(b'/');
                buf.extend_from_slice(MONTHS[self.mo as usize - 1].as_bytes());
                buf.push(b'/');
                push_pad(buf, y, 4);
                buf.push(b':');
                push_pad(buf, h, 2);
                buf.push(b':');
                push_pad(buf, mi, 2);
                buf.push(b':');
                push_pad(buf, s, 2);
                buf.extend_from_slice(b" +0000");
            }
            Ts::Syslog => {
                buf.extend_from_slice(MONTHS[self.mo as usize - 1].as_bytes());
                buf.push(b' ');
                if d < 10 {
                    buf.push(b' ');
                }
                push_u(buf, d);
                buf.push(b' ');
                push_pad(buf, h, 2);
                buf.push(b':');
                push_pad(buf, mi, 2);
                buf.push(b':');
                push_pad(buf, s, 2);
            }
            Ts::IisDate => {
                push_pad(buf, y, 4);
                buf.push(b'-');
                push_pad(buf, mo, 2);
                buf.push(b'-');
                push_pad(buf, d, 2);
            }
            Ts::IisTime => {
                push_pad(buf, h, 2);
                buf.push(b':');
                push_pad(buf, mi, 2);
                buf.push(b':');
                push_pad(buf, s, 2);
            }
        }
    }

    fn advance(&mut self) {
        let step = self.step_mean * (0.2 + 1.6 * self.rng.f64());
        self.micros += step as i64;
        self.refresh_civil();
    }

    fn level(&mut self) -> usize {
        match self.rng.u32(..1000) {
            0..=819 => 2,
            820..=899 => 1,
            900..=959 => 3,
            960..=989 => 4,
            990..=994 => 0,
            995..=997 => 4,
            _ => 5,
        }
    }

    fn eol(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(self.eol);
    }

    fn ip(&mut self, buf: &mut Vec<u8>) {
        let a = match self.rng.u32(..4) {
            0 => 10,
            1 => 192,
            _ => 20 + self.rng.u64(..200),
        };
        push_u(buf, a);
        buf.push(b'.');
        push_u(buf, if a == 192 { 168 } else { self.rng.u64(..256) });
        buf.push(b'.');
        push_u(buf, self.rng.u64(..256));
        buf.push(b'.');
        push_u(buf, 1 + self.rng.u64(..254));
    }

    fn path(&mut self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(PATHS[self.rng.usize(..PATHS.len())].as_bytes());
        if self.rng.u32(..5) == 0 {
            buf.push(b'/');
            push_u(buf, self.rng.u64(..100_000));
        }
    }

    fn num(&mut self, buf: &mut Vec<u8>, max: u64) {
        push_u(buf, self.rng.u64(..max));
    }

    /// Free-text application message (ASCII plus a few latin1-representable chars, no quotes).
    fn message(&mut self, buf: &mut Vec<u8>, level: usize) {
        match level {
            4 | 5 => match self.rng.u32(..5) {
                0 => {
                    buf.extend_from_slice(b"Failed to connect to db-");
                    self.num(buf, 8);
                    buf.extend_from_slice(b": connection refused");
                }
                1 => {
                    buf.extend_from_slice(b"Timeout after ");
                    self.num(buf, 30_000);
                    buf.extend_from_slice(b"ms calling upstream billing-service");
                }
                2 => {
                    buf.extend_from_slice(b"Unhandled exception while handling request ");
                    self.num(buf, 10_000_000);
                }
                3 => {
                    buf.extend_from_slice(b"Payment declined for order ");
                    self.num(buf, 1_000_000);
                    buf.extend_from_slice(b" reason=insufficient_funds");
                }
                _ => {
                    buf.extend_from_slice(b"Disk write failed on /var/lib/data errno=");
                    self.num(buf, 40);
                }
            },
            3 => match self.rng.u32(..3) {
                0 => {
                    buf.extend_from_slice(b"Slow query detected duration=");
                    self.num(buf, 5000);
                    buf.extend_from_slice(b"ms table=orders");
                }
                1 => {
                    buf.extend_from_slice(b"Retrying request attempt=");
                    self.num(buf, 5);
                    buf.extend_from_slice(b" upstream=inventory");
                }
                _ => {
                    buf.extend_from_slice(b"Connection pool nearly exhausted active=");
                    self.num(buf, 100);
                    buf.extend_from_slice(b" max=100");
                }
            },
            _ => match self.rng.u32(..9) {
                0 | 1 => {
                    buf.extend_from_slice(b"Request completed method=");
                    buf.extend_from_slice(METHODS[self.rng.usize(..METHODS.len())].as_bytes());
                    buf.extend_from_slice(b" path=");
                    self.path(buf);
                    buf.extend_from_slice(b" status=");
                    push_u(buf, u64::from(STATUSES[self.rng.usize(..STATUSES.len())]));
                    buf.extend_from_slice(b" duration=");
                    self.num(buf, 900);
                    buf.extend_from_slice(b"ms");
                }
                2 => {
                    buf.extend_from_slice(b"User u");
                    self.num(buf, 100_000);
                    buf.extend_from_slice(b" logged in from ");
                    self.ip(buf);
                }
                3 => {
                    buf.extend_from_slice(b"Cache miss for key session:");
                    self.num(buf, 1_000_000);
                }
                4 => {
                    buf.extend_from_slice(b"Processed batch id=");
                    self.num(buf, 1_000_000);
                    buf.extend_from_slice(b" items=");
                    self.num(buf, 5000);
                    buf.extend_from_slice(b" in ");
                    self.num(buf, 2000);
                    buf.extend_from_slice(b"ms");
                }
                5 => {
                    buf.extend_from_slice(b"Scheduled job cleanup finished, removed ");
                    self.num(buf, 10_000);
                    buf.extend_from_slice(b" rows");
                }
                6 => {
                    buf.extend_from_slice(b"Payment received for order ");
                    self.num(buf, 1_000_000);
                    buf.extend_from_slice(b" amount=");
                    self.num(buf, 500);
                    buf.push(b'.');
                    push_pad(buf, self.rng.u64(..100), 2);
                    buf.extend_from_slice(" EUR caf\u{e9} n\u{b0}".as_bytes());
                    self.num(buf, 50);
                }
                7 => {
                    buf.extend_from_slice(
                        "R\u{e9}servation cr\u{e9}\u{e9}e pour l'utilisateur u".as_bytes(),
                    );
                    self.num(buf, 100_000);
                }
                _ => {
                    buf.extend_from_slice(b"Health check ok latency=");
                    self.num(buf, 50);
                    buf.extend_from_slice(b"ms");
                }
            },
        }
    }

    fn stack_trace(&mut self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(EXCEPTIONS[self.rng.usize(..EXCEPTIONS.len())].as_bytes());
        self.eol(buf);
        let depth = 3 + self.rng.usize(..6);
        for _ in 0..depth {
            let c = CLASSES[self.rng.usize(..CLASSES.len())];
            let simple = c.rsplit('.').next().unwrap_or(c);
            buf.extend_from_slice(b"\tat ");
            buf.extend_from_slice(c.as_bytes());
            buf.extend_from_slice(b".handle(");
            buf.extend_from_slice(simple.as_bytes());
            buf.extend_from_slice(b".java:");
            self.num(buf, 900);
            buf.push(b')');
            self.eol(buf);
        }
        if self.rng.bool() {
            buf.extend_from_slice(b"Caused by: ");
            buf.extend_from_slice(EXCEPTIONS[self.rng.usize(..EXCEPTIONS.len())].as_bytes());
            self.eol(buf);
            buf.extend_from_slice(b"\t... ");
            self.num(buf, 40);
            buf.extend_from_slice(b" more");
            self.eol(buf);
        }
    }

    /// Append any file header lines (`#Fields:` for iis, column row for csv).
    pub fn header(&mut self, buf: &mut Vec<u8>) {
        match self.format {
            Format::Csv => {
                buf.extend_from_slice(b"timestamp,level,user_id,status,duration_ms,message");
                self.eol(buf);
            }
            Format::Iis => {
                buf.extend_from_slice(b"#Software: Microsoft Internet Information Services 10.0");
                self.eol(buf);
                buf.extend_from_slice(b"#Version: 1.0");
                self.eol(buf);
                buf.extend_from_slice(b"#Date: ");
                self.ts(buf, Ts::IisDate);
                buf.push(b' ');
                self.ts(buf, Ts::IisTime);
                self.eol(buf);
                buf.extend_from_slice(b"#Fields: date time s-ip cs-method cs-uri-stem cs-uri-query s-port cs-username c-ip cs(User-Agent) cs(Referer) sc-status sc-substatus sc-win32-status time-taken");
                self.eol(buf);
            }
            _ => {}
        }
    }

    /// Append one log entry (possibly several physical lines).
    pub fn line(&mut self, buf: &mut Vec<u8>) {
        self.advance();
        let mut level = self.level();
        let multiline = matches!(
            self.format,
            Format::Plain | Format::Log4j | Format::Logfmt | Format::Syslog | Format::Syslog5424
        ) && self.multiline_ratio > 0.0
            && self.rng.f64() < self.multiline_ratio;
        if multiline {
            level = 4;
        }
        match self.format {
            Format::Plain => {
                self.ts(buf, Ts::Plain);
                buf.push(b' ');
                buf.extend_from_slice(LEVELS[level].as_bytes());
                buf.extend_from_slice(if level == 2 || level == 4 {
                    b"  "
                } else {
                    b" "
                });
                if level == 3 || level == 5 || level == 1 {
                    // keep the message column roughly aligned: WARN/DEBUG/FATAL are 4-5 chars
                }
                self.message(buf, level);
                self.eol(buf);
            }
            Format::Log4j => {
                self.ts(buf, Ts::Comma);
                buf.push(b' ');
                buf.extend_from_slice(LEVELS[level].as_bytes());
                buf.extend_from_slice(if LEVELS[level].len() == 4 {
                    b"  ["
                } else {
                    b" ["
                });
                buf.extend_from_slice(THREADS[self.rng.usize(..THREADS.len())].as_bytes());
                buf.extend_from_slice(b"] ");
                buf.extend_from_slice(CLASSES[self.rng.usize(..CLASSES.len())].as_bytes());
                buf.extend_from_slice(b" - ");
                self.message(buf, level);
                self.eol(buf);
            }
            Format::Nginx | Format::Apache => {
                self.ip(buf);
                buf.extend_from_slice(b" - ");
                if self.rng.u32(..4) == 0 {
                    buf.extend_from_slice(b"u");
                    self.num(buf, 5000);
                } else {
                    buf.push(b'-');
                }
                buf.extend_from_slice(b" [");
                self.ts(buf, Ts::Clf);
                buf.extend_from_slice(b"] \"");
                buf.extend_from_slice(METHODS[self.rng.usize(..METHODS.len())].as_bytes());
                buf.push(b' ');
                self.path(buf);
                buf.extend_from_slice(b" HTTP/1.1\" ");
                let status = STATUSES[self.rng.usize(..STATUSES.len())];
                push_u(buf, u64::from(status));
                buf.push(b' ');
                if status == 204 || status == 304 {
                    buf.push(b'0');
                } else {
                    self.num(buf, 50_000);
                }
                buf.extend_from_slice(b" \"");
                buf.extend_from_slice(REFERERS[self.rng.usize(..REFERERS.len())].as_bytes());
                buf.extend_from_slice(b"\" \"");
                buf.extend_from_slice(AGENTS[self.rng.usize(..AGENTS.len())].as_bytes());
                buf.push(b'"');
                if self.format == Format::Apache {
                    // %D: request time in microseconds
                    buf.push(b' ');
                    self.num(buf, 900_000);
                } else {
                    buf.extend_from_slice(b" rt=0.");
                    push_pad(buf, self.rng.u64(..1000), 3);
                }
                self.eol(buf);
            }
            Format::Jsonl => {
                buf.extend_from_slice(b"{\"ts\":\"");
                self.ts(buf, Ts::Rfc3339);
                buf.extend_from_slice(b"\",\"level\":\"");
                buf.extend_from_slice(LEVELS[level].as_bytes());
                buf.extend_from_slice(b"\",\"user_id\":");
                self.num(buf, 100_000);
                buf.extend_from_slice(b",\"status\":");
                push_u(buf, u64::from(STATUSES[self.rng.usize(..STATUSES.len())]));
                buf.extend_from_slice(b",\"duration_ms\":");
                self.num(buf, 2000);
                buf.extend_from_slice(b",\"msg\":\"");
                self.message(buf, level);
                buf.extend_from_slice(b"\"}");
                self.eol(buf);
            }
            Format::Logfmt => {
                buf.extend_from_slice(b"ts=");
                self.ts(buf, Ts::Rfc3339);
                buf.extend_from_slice(b" level=");
                buf.extend_from_slice(LEVELS_LOWER[level].as_bytes());
                buf.extend_from_slice(b" host=");
                buf.extend_from_slice(HOSTS[self.rng.usize(..HOSTS.len())].as_bytes());
                buf.extend_from_slice(b" user_id=");
                self.num(buf, 100_000);
                buf.extend_from_slice(b" duration_ms=");
                self.num(buf, 2000);
                buf.extend_from_slice(b" msg=\"");
                self.message(buf, level);
                buf.push(b'"');
                self.eol(buf);
            }
            Format::Syslog => {
                self.ts(buf, Ts::Syslog);
                buf.push(b' ');
                buf.extend_from_slice(HOSTS[self.rng.usize(..HOSTS.len())].as_bytes());
                buf.push(b' ');
                buf.extend_from_slice(PROCS[self.rng.usize(..PROCS.len())].as_bytes());
                buf.push(b'[');
                self.num(buf, 32_768);
                buf.extend_from_slice(b"]: ");
                self.message(buf, level);
                self.eol(buf);
            }
            Format::Syslog5424 => {
                // facility 16 (local0) * 8 + severity
                let sev = [7u64, 7, 6, 4, 3, 2][level];
                buf.push(b'<');
                push_u(buf, 128 + sev);
                buf.extend_from_slice(b">1 ");
                self.ts(buf, Ts::Rfc3339);
                buf.push(b' ');
                buf.extend_from_slice(HOSTS[self.rng.usize(..HOSTS.len())].as_bytes());
                buf.push(b' ');
                buf.extend_from_slice(PROCS[self.rng.usize(..PROCS.len())].as_bytes());
                buf.push(b' ');
                self.num(buf, 32_768);
                buf.extend_from_slice(b" ID");
                self.num(buf, 500);
                buf.extend_from_slice(b" [meta@32473 seq=\"");
                self.num(buf, 1_000_000);
                buf.extend_from_slice(b"\"] ");
                self.message(buf, level);
                self.eol(buf);
            }
            Format::Csv => {
                self.ts(buf, Ts::Rfc3339);
                buf.push(b',');
                buf.extend_from_slice(LEVELS[level].as_bytes());
                buf.push(b',');
                self.num(buf, 100_000);
                buf.push(b',');
                push_u(buf, u64::from(STATUSES[self.rng.usize(..STATUSES.len())]));
                buf.push(b',');
                self.num(buf, 2000);
                buf.extend_from_slice(b",\"");
                self.message(buf, level);
                buf.push(b'"');
                self.eol(buf);
            }
            Format::Iis => {
                self.ts(buf, Ts::IisDate);
                buf.push(b' ');
                self.ts(buf, Ts::IisTime);
                buf.extend_from_slice(b" 10.0.0.");
                push_u(buf, 1 + self.rng.u64(..20));
                buf.push(b' ');
                buf.extend_from_slice(METHODS[self.rng.usize(..METHODS.len())].as_bytes());
                buf.push(b' ');
                self.path(buf);
                if self.rng.u32(..4) == 0 {
                    buf.extend_from_slice(b" page=");
                    self.num(buf, 50);
                } else {
                    buf.extend_from_slice(b" -");
                }
                buf.extend_from_slice(b" 443 ");
                if self.rng.u32(..4) == 0 {
                    buf.push(b'u');
                    self.num(buf, 5000);
                } else {
                    buf.push(b'-');
                }
                buf.push(b' ');
                self.ip(buf);
                buf.push(b' ');
                // IIS escapes spaces in the user agent as '+'
                let ua = AGENTS[self.rng.usize(..AGENTS.len())];
                buf.extend(ua.bytes().map(|b| if b == b' ' { b'+' } else { b }));
                buf.push(b' ');
                buf.extend_from_slice(REFERERS[self.rng.usize(..REFERERS.len())].as_bytes());
                buf.push(b' ');
                push_u(buf, u64::from(STATUSES[self.rng.usize(..STATUSES.len())]));
                buf.extend_from_slice(b" 0 0 ");
                self.num(buf, 3000);
                self.eol(buf);
            }
        }
        if multiline {
            self.stack_trace(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_lines(f: Format, n: usize, seed: u64) -> Vec<u8> {
        let mut g = Generator::new(f, seed, 1_704_067_200_000_000, 100.0, 0.05, false);
        let mut b = Vec::new();
        g.header(&mut b);
        for _ in 0..n {
            g.line(&mut b);
        }
        b
    }

    const ALL: [Format; 10] = [
        Format::Plain,
        Format::Nginx,
        Format::Apache,
        Format::Jsonl,
        Format::Logfmt,
        Format::Log4j,
        Format::Syslog,
        Format::Syslog5424,
        Format::Csv,
        Format::Iis,
    ];

    #[test]
    fn deterministic_and_valid_utf8() {
        for f in ALL {
            let a = gen_lines(f, 500, 7);
            assert_eq!(a, gen_lines(f, 500, 7), "{f:?}");
            assert_ne!(a, gen_lines(f, 500, 8), "{f:?}");
            assert!(std::str::from_utf8(&a).is_ok(), "{f:?}");
        }
    }

    #[test]
    fn parse_time_and_size() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            parse_rfc3339("2024-01-01T01:00:00.5+01:00").unwrap(),
            1_704_067_200_500_000
        );
        assert!(parse_rfc3339("nope").is_err());
        assert_eq!(parse_size("500M").unwrap(), 500 << 20);
        assert_eq!(parse_size("10g").unwrap(), 10 << 30);
        assert_eq!(parse_size("7").unwrap(), 7);
        assert!(parse_size("x").is_err());
    }

    #[test]
    fn timestamps_monotonic_plain() {
        let s = String::from_utf8(gen_lines(Format::Plain, 2000, 1)).unwrap();
        let mut prev = "";
        for l in s.lines().filter(|l| l.starts_with("20")) {
            let t = &l[..23];
            assert!(t >= prev, "{t} < {prev}");
            prev = t;
        }
    }

    #[test]
    fn iis_and_csv_headers() {
        let s = String::from_utf8(gen_lines(Format::Iis, 3, 1)).unwrap();
        assert!(s.contains("#Fields: date time"));
        let s = String::from_utf8(gen_lines(Format::Csv, 3, 1)).unwrap();
        assert!(s.starts_with("timestamp,level,"));
    }

    #[test]
    fn encodings() {
        let mut out = Vec::new();
        Encoding::Utf16Le.encode("a\u{e9}".as_bytes(), &mut out);
        assert_eq!(out, [b'a', 0, 0xE9, 0]);
        Encoding::Latin1.encode("a\u{e9}\u{4e2d}".as_bytes(), &mut out);
        assert_eq!(out, [b'a', 0xE9, b'?']);
    }
}
