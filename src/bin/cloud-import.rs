fn main() -> promptlist::Result<()> {
    let path = std::env::args()
        .nth(1)
        .ok_or_else(|| promptlist::message("Specify the local read-only database path."))?;
    let (users, rows) = promptlist::cloud::import_local(
        promptlist::cloud::Aws::env()?,
        std::path::Path::new(&path),
    )?;
    println!(
        "Migrated {users} account(s), {rows} private rows. Local database unchanged; no tokens migrated."
    );
    Ok(())
}
