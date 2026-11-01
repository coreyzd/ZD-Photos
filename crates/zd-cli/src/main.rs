//! `zdp` — drive the ZD-Photos core from a terminal while the Qt app is being built.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use zd_catalog::{Catalog, VolumeKind};
use zd_import::{card, Progress, ThumbCache};

#[derive(Parser)]
#[command(name = "zdp", version, about = "ZD-Photos command line")]
struct Cli {
    /// Data folder holding catalog.sqlite and the thumbnail cache
    /// (default: the per-user app data folder; or set ZDP_DATA).
    #[arg(long, global = true, env = "ZDP_DATA")]
    data: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Detect drives, shares and SD cards and update the catalog.
    Drives,
    /// Set the Home Library network share, e.g. smb://nobara-zd.local/Photos/
    Home {
        share: String,
        /// On the computer serving the share: the local folder it exports (normally found
        /// automatically from the Samba config).
        #[arg(long)]
        local: Option<PathBuf>,
    },
    /// Catalogue all photos under a folder (or a whole drive).
    Scan { folder: PathBuf },
    /// Import new photos from an SD card into a folder.
    Card {
        /// Where to copy them (dated sub-folders are created).
        #[arg(long)]
        to: PathBuf,
        /// Card root; default: the first SD card found.
        card: Option<PathBuf>,
        /// Only show what would be imported.
        #[arg(long)]
        dry_run: bool,
        /// Delete from the card the photos whose copies were verified.
        #[arg(long)]
        clear: bool,
    },
    /// List photos, newest first.
    Photos {
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
    /// Everything known about one photo.
    Info { id: i64 },
    /// Add a tag like "Birds/Great Blue Heron" to a photo.
    Tag { id: i64, tag: String },
    /// Catalog totals.
    Stats,
}

fn data_dir(cli: &Cli) -> Result<PathBuf> {
    if let Some(d) = &cli.data {
        return Ok(d.clone());
    }
    let dirs = directories::ProjectDirs::from("com", "ZD", "ZD-Photos")
        .context("can't determine the app data folder; pass --data")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn progress_bar(p: &Progress) {
    let name = p.current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    eprint!("\r  {}/{}  {:<40.40}", p.done, p.total, name);
    let _ = std::io::stderr().flush();
}

fn refresh(catalog: &Catalog) -> Result<()> {
    let mounts = zd_volumes::list_mounts();
    zd_volumes::refresh(catalog, &mounts)?;
    zd_volumes::home::connect(catalog, &mounts)?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data = data_dir(&cli)?;
    let catalog = Catalog::open(data.join("catalog.sqlite"))
        .with_context(|| format!("opening catalog in {}", data.display()))?;
    let cache = ThumbCache::new(data.join("cache"));

    match cli.cmd {
        Cmd::Drives => {
            refresh(&catalog)?;
            println!("id   status   kind       name                     where");
            for v in catalog.volumes()? {
                let status = if v.online { "online" } else { "offline" };
                let name = if v.is_home_library { format!("⌂ {}", v.label) } else { v.label.clone() };
                let place = v.last_mount.clone().unwrap_or_default();
                println!("{:<4} {:<8} {:<10} {:<24} {}", v.id, status, v.kind.as_str(), name, place);
            }
        }
        Cmd::Home { share, local } => {
            zd_volumes::home::configure(&catalog, &share)?;
            zd_volumes::home::set_local_path(&catalog, local.as_deref())?;
            refresh(&catalog)?;
            match catalog.home_library()?.filter(|v| v.online) {
                Some(v) => println!("Home Library connected at {}", v.last_mount.unwrap_or_default()),
                None => println!(
                    "Saved, but the share isn't reachable from here yet. Either mount it (fstab cifs \
                     entry, or kio-fuse for Dolphin), or on the server pass --local /path/it/exports."
                ),
            }
        }
        Cmd::Scan { folder } => {
            refresh(&catalog)?;
            let r = zd_import::scan(&catalog, &cache, &folder, &mut progress_bar)?;
            eprintln!();
            println!(
                "{} photo files: {} new photos, {} new locations, {} unchanged, {} thumbnails made, {} now missing",
                r.files_found, r.new_photos, r.new_locations, r.unchanged, r.thumbnails_made, r.missing
            );
            for (p, e) in r.failed.iter().take(20) {
                println!("  ! {}: {e}", p.display());
            }
        }
        Cmd::Card { to, card: card_root, dry_run, clear } => {
            refresh(&catalog)?;
            let root = match card_root {
                Some(r) => r,
                None => catalog
                    .volumes()?
                    .into_iter()
                    .find(|v| v.online && v.kind == VolumeKind::Card)
                    .and_then(|v| v.last_mount.map(PathBuf::from))
                    .context("no SD card found; pass the card folder explicitly")?,
            };
            let plan = card::plan(&catalog, &root)?;
            let new = plan.iter().filter(|i| !i.already_imported).count();
            println!("{} photos on the card, {} new", plan.len(), new);
            if dry_run || new == 0 {
                return Ok(());
            }
            let rep = card::import(&catalog, &cache, &plan, &to, &mut progress_bar)?;
            eprintln!();
            println!("Copied and verified {} photos into {}", rep.copied.len(), to.display());
            for (p, e) in &rep.failed {
                println!("  ! {}: {e}", p.display());
            }
            if clear {
                if !rep.failed.is_empty() {
                    bail!("not clearing the card: some photos failed to copy");
                }
                println!("Cleared {} photos from the card", card::clear_verified(&rep));
            }
        }
        Cmd::Photos { limit } => {
            for p in catalog.photos(limit, 0)? {
                let backed = if catalog.is_backed_up(p.id)? { "✓" } else { " " };
                println!(
                    "{:>6} {} {:<19} {:<18} {:>5}x{:<5} {}",
                    p.id,
                    backed,
                    p.meta.taken_at.as_deref().unwrap_or("-"),
                    p.meta.camera_model.as_deref().unwrap_or("-"),
                    p.meta.width.unwrap_or(0),
                    p.meta.height.unwrap_or(0),
                    p.orientation_class().unwrap_or("")
                );
            }
        }
        Cmd::Info { id } => {
            let p = catalog.photo(id)?.context("no such photo")?;
            println!("{}", serde_json::to_string_pretty(&p)?);
            println!("tags: {:?}", catalog.photo_tags(id)?);
            println!("backed up: {}", catalog.is_backed_up(id)?);
            println!("thumbnail: {}", cache.path(&p.fingerprint, zd_import::Size::Thumb).display());
            for l in catalog.locations(id)? {
                let state = match (l.online, l.missing) {
                    (_, true) => "missing",
                    (true, false) => "online",
                    (false, false) => "offline",
                };
                println!("  [{state}] {} : {}", l.volume_label, l.rel_path);
            }
        }
        Cmd::Tag { id, tag } => {
            catalog.photo(id)?.context("no such photo")?;
            let t = catalog.tag_path(&tag)?;
            catalog.tag_photo(id, t)?;
            println!("tags: {:?}", catalog.photo_tags(id)?);
        }
        Cmd::Stats => {
            println!("{}", serde_json::to_string_pretty(&catalog.stats()?)?);
        }
    }
    Ok(())
}
