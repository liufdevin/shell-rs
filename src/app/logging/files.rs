use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::{Days, NaiveDate};

const RETENTION_DAYS: u64 = 7;
const FILE_COUNT: usize = 5;
const FILE_BYTES: u64 = 10 * 1024 * 1024;

pub(super) struct DailyLog {
    directory: PathBuf,
    date: NaiveDate,
    process: u32,
    file: Option<File>,
    bytes: u64,
    limit: u64,
}

impl DailyLog {
    pub(super) fn new(directory: PathBuf, date: NaiveDate, process: u32) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        cleanup(&directory, date);
        let mut log = Self {
            directory,
            date,
            process,
            file: None,
            bytes: 0,
            limit: FILE_BYTES,
        };
        log.open()?;
        Ok(log)
    }

    fn path(&self, part: usize) -> PathBuf {
        let suffix = if part == 0 {
            String::new()
        } else {
            format!(".{part}")
        };
        self.directory.join(format!(
            "shelllf-{}-{}{suffix}.log",
            self.date, self.process
        ))
    }

    fn open(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(0))?;
        self.bytes = file.metadata()?.len();
        self.file = Some(file);
        Ok(())
    }

    pub(super) fn maintain(&mut self, date: NaiveDate) -> io::Result<()> {
        if self.date != date {
            self.file = None;
            self.date = date;
            self.bytes = 0;
            cleanup(&self.directory, date);
        }
        if self.file.is_none() {
            self.open()?;
        }
        Ok(())
    }

    pub(super) fn write(&mut self, date: NaiveDate, record: &[u8]) -> io::Result<()> {
        self.maintain(date)?;
        if self.bytes > 0 && self.bytes.saturating_add(record.len() as u64) > self.limit {
            self.rotate()?;
        }
        let result = self.file.as_mut().unwrap().write_all(record);
        if result.is_err() {
            self.file = None;
        } else {
            self.bytes += record.len() as u64;
        }
        result
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        match fs::remove_file(self.path(FILE_COUNT - 1)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for part in (0..FILE_COUNT - 1).rev() {
            match fs::rename(self.path(part), self.path(part + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.open()
    }

    pub(super) fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

fn log_date(name: &str) -> Option<NaiveDate> {
    let body = name.strip_prefix("shelllf-")?.strip_suffix(".log")?;
    let date = body.get(..10)?;
    let process = body.get(10..)?.strip_prefix('-')?;
    let (process, part) = process
        .split_once('.')
        .map_or((process, None), |(process, part)| (process, Some(part)));
    let is_number = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u32>().is_ok_and(|value| value > 0)
    };
    if !is_number(process) || part.is_some_and(|part| !is_number(part)) {
        return None;
    }
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    (parsed.to_string() == date).then_some(parsed)
}

fn cleanup(directory: &Path, today: NaiveDate) {
    let Some(cutoff) = today.checked_sub_days(Days::new(RETENTION_DAYS - 1)) else {
        return;
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("ShellLF: cannot inspect old application logs: {error}");
            return;
        }
    };
    for entry in entries {
        let result = entry.and_then(|entry| {
            if entry.file_type()?.is_file()
                && entry
                    .file_name()
                    .to_str()
                    .and_then(log_date)
                    .is_some_and(|date| date < cutoff)
            {
                fs::remove_file(entry.path())?;
            }
            Ok(())
        });
        if let Err(error) = result {
            eprintln!("ShellLF: cannot remove an expired application log: {error}");
        }
    }
}

#[cfg(test)]
mod tests;
