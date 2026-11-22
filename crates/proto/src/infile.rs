//! `LOAD DATA LOCAL INFILE`: which files a command may send, opening one
//! the server asks for, and what the statement then reports. Shared by
//! whoever drives the connection; sending the file is theirs.

use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use crate::error::Error;
use crate::error::Result;
use crate::result::QueryResult;

/// Bytes of a local file sent per packet (`LOAD DATA LOCAL INFILE`). Measured
/// from 4 to 64 KiB (benches/README.md): flat on a local link, while over a
/// slow compressed one 16 KiB was 5% faster with zstd than 64 KiB.
pub(crate) const CHUNK: usize = 16 * 1024;

/// Which local files the server may ask for during the current command.
#[derive(Debug)]
pub(crate) enum Infile {
    /// None.
    Refuse,
    /// Those the statement names (`LOAD DATA LOCAL INFILE 'name'`).
    Allow(Vec<Vec<u8>>),
    /// None, and a request breaks the connection: the server would read
    /// the statements sent after this one as the file's contents.
    Pipelined,
}

/// Opens the local file `name` (as the server asked for it) if this
/// command may send it, or says why not. Nothing on disk is touched
/// unless the statement named the file.
pub(crate) fn open(
    dir: Option<&Path>,
    infile: &Infile,
    name: &[u8],
) -> std::result::Result<(PathBuf, File), String> {
    let shown = String::from_utf8_lossy(name);
    let Some(dir) = dir else {
        return Err(format!(
            "the server asked for {shown:?}, but LOAD DATA LOCAL INFILE is disabled \
             (local_infile_dir allows the files of a directory)"
        ));
    };
    let named = match infile {
        Infile::Allow(names) => names.iter().any(|allowed| allowed == name),
        Infile::Refuse | Infile::Pipelined => false,
    };
    if !named {
        return Err(format!(
            "the server asked for {shown:?}, which the statement being run does not name"
        ));
    }
    let path = std::str::from_utf8(name)
        .map_err(|_| format!("the file name {shown:?} is not valid UTF-8"))?;
    // Relative names start at the working directory, as in other clients.
    let resolved = fs::canonicalize(path).map_err(|err| format!("{path}: {err}"))?;
    if !resolved.starts_with(dir) {
        return Err(format!(
            "{path} is outside local_infile_dir ({})",
            dir.display()
        ));
    }
    // Checked before opening too: opening a FIFO blocks until something
    // writes to it, with the connection held. The descriptor is checked
    // again below, in case the path changed in between.
    let is_file = fs::metadata(&resolved)
        .map_err(|err| format!("{path}: {err}"))?
        .is_file();
    if !is_file {
        return Err(format!("{path} is not a regular file"));
    }
    let file = File::open(&resolved).map_err(|err| format!("{path}: {err}"))?;
    let is_file = file
        .metadata()
        .map_err(|err| format!("{path}: {err}"))?
        .is_file();
    if !is_file {
        return Err(format!("{path} is not a regular file"));
    }
    Ok((resolved, file))
}

/// Fills `buf` from `file` unless it ends first; returns the bytes read.
pub(crate) fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        match file.read(rest) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// What the statement reports once the server's file request was
/// answered: `refused` says why an empty file went instead, `reply` is the
/// server's answer. A broken connection wins; otherwise a refusal does.
pub(crate) fn outcome(refused: Option<String>, reply: Result<QueryResult>) -> Result<QueryResult> {
    match (refused, reply) {
        (_, Err(err @ (Error::Io(_) | Error::Protocol(_)))) | (None, Err(err)) => Err(err),
        (Some(reason), _) => Err(Error::LocalInfile(reason)),
        (None, reply) => reply,
    }
}
