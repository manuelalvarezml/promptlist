//! Offline operator tool: no web endpoint, no credentials, no external service.
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{fs::OpenOptions, io::Write, path::PathBuf};

fn main() {
    if run().is_err() {
        eprintln!(
            "Export failed. Use: audit-export <database> <new-private-output.json>. Output must not already exist; database must be initialized by Promptlist."
        );
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("arguments".into());
    }
    let db_path = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    export_to(db_path, output)
}

fn export_to(db_path: PathBuf, output: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut db =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let data = promptlist::history::export(&mut db)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&output)?;
    serde_json::to_writer_pretty(&mut file, &data)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    println!(
        "Private audit export written. Contains personal prompts and playlist results; keep it local."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readonly_export_is_private_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fixture.sqlite");
        let output = dir.path().join("export.json");
        let db = rusqlite::Connection::open(&source).unwrap();
        promptlist::history::init(&db).unwrap();
        promptlist::history::ensure_user(&db, "private-account").unwrap();
        export_to(source.clone(), output.clone()).unwrap();
        let data = std::fs::read_to_string(&output).unwrap();
        assert!(!data.contains("private-account"));
        assert!(data.contains("schema_version"));
        assert!(export_to(source, output.clone()).is_err());
        assert_eq!(std::fs::read_to_string(&output).unwrap(), data);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(output).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
