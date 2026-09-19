use std::{env, fmt, fs};

#[derive(Debug)]
struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

struct Config {
    path: String,
    retries: u32,
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} retries)", self.path, self.retries)
    }
}

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn parse(s: &str) -> Result<Config, Error> {
    let mut parts = s.lines();
    let path = parts
        .next()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| Error("missing path".into()))?;
    let retries = parts
        .next()
        .unwrap_or("3")
        .parse()
        .map_err(|_| Error("retries must be a number".into()))?;
    Ok(Config {
        path: path.into(),
        retries,
    })
}

fn validate(c: &Config) -> Result<(), Error> {
    if c.retries > 10 {
        return Err(Error("retries must not exceed 10".into()));
    }
    Ok(())
}

fn normalize(s: &str) -> String {
    s.trim().to_owned()
}

fn log(s: &str) {
    eprintln!("loaded {s}");
}

fn canon(c: &Config) -> Config {
    Config {
        path: c.path.trim().to_owned(),
        retries: c.retries,
    }
}

fn load(path: &str) -> Result<Config, Error> {
    let raw = read(path);
    let cfg = parse(&raw)?;
    validate(&cfg)?;
    let _path_exists = !cfg.path.is_empty();
    let _retry_count = cfg.retries;
    Ok(cfg)
}

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| "config.txt".into());
    match load(&path) {
        Ok(config) => println!("{config}"),
        Err(error) => eprintln!("error: {error}"),
    }
}
