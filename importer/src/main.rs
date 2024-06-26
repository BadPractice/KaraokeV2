use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{metadata, read_dir},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use clap::Parser;
use rusqlite::{Connection, OpenFlags, Statement};
use ultrastar_txt::{loader, Source};

#[derive(Parser, Debug)]
struct Args {
    path: PathBuf,

    /// The path to the sqlite database to write the output to. Will be created if it doesn't exist.
    #[clap(short, long)]
    db: PathBuf,

    /// How many path components to remove from media paths to match the web server configuration.
    #[clap(short, long, default_value_t = 0)]
    strip_components: usize,
}

fn parse_txt(
    path: impl AsRef<Path>,
    strip_components: usize,
    insert_stmt: &mut Statement<'_>,
    inserted_set: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    let full_path = path.as_ref().canonicalize()?;
    let song = loader::parse_txt_song(&path).map_err(|err| anyhow::anyhow!("{err:?}"))?;

    let Source::Local(audio_path) = &song.header.audio_path else {
        return Err(anyhow::anyhow!(
            "{:?} does not have a local audio track.",
            path.as_ref()
        ));
    };

    let context = ffmpeg_next::format::input(&audio_path)?;
    let Some(stream) = context.streams().best(ffmpeg_next::media::Type::Audio) else {
        return Err(anyhow::anyhow!(
            "{:?} does not contain an audio track.",
            path.as_ref()
        ));
    };

    let cover_path = song.header.cover_path.map(|cover_path| match cover_path {
        Source::Local(cover_path) => cover_path
            .components()
            .skip(strip_components)
            .collect::<PathBuf>()
            .as_os_str()
            .as_bytes()
            .to_owned(),
        _ => panic!("Song {} has remote cover", song.header.title),
    });

    let audio_path = match &song.header.audio_path {
        Source::Local(audio_path) => audio_path
            .components()
            .skip(strip_components)
            .collect::<PathBuf>()
            .as_os_str()
            .as_bytes()
            .to_owned(),
        _ => panic!("Song {} has remote audio", song.header.title),
    };

    let player_count = (song.lines.iter().any(|line| {
        line.notes.iter().any(|note| match note {
            ultrastar_txt::Note::PlayerChange { player } => *player == 2,
            _ => false,
        })
    }) as u32)
        + 1;

    let changes = insert_stmt.execute((
        full_path.as_os_str().as_bytes(),
        song.header.title.trim(),
        song.header.artist.trim(),
        song.header.language.map(|lang| lang.trim().to_owned()),
        song.header.year,
        stream.duration() as f64 * f64::from(stream.time_base()),
        song.lines
            .into_iter()
            .map(|line| {
                line.notes
                    .into_iter()
                    .filter_map(|note| match note {
                        ultrastar_txt::Note::Regular { text, .. } => Some(text),
                        ultrastar_txt::Note::Golden { text, .. } => Some(text),
                        ultrastar_txt::Note::Freestyle { text, .. } => Some(text),
                        ultrastar_txt::Note::PlayerChange { .. } => None,
                    })
                    .collect::<String>()
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        player_count,
        cover_path,
        audio_path,
    ))?;

    if changes == 1 {
        inserted_set.insert(full_path);
    } else {
        eprintln!("{full_path:?}: Failed inserting into database");
    }

    Ok(())
}

fn walk_dir(
    path: impl AsRef<Path>,
    strip_components: usize,
    insert_stmt: &mut Statement<'_>,
    inserted_set: &mut HashSet<PathBuf>,
) -> anyhow::Result<()> {
    for subdir in read_dir(path)? {
        let subdir = subdir?;

        // Retrieve metadata based on the actual file or symlink.
        let file_type = subdir.file_type()?;

        if file_type.is_dir() || (file_type.is_symlink() && metadata(subdir.path())?.is_dir()) {
            // Recursive call if it's a directory or a symlink pointing to a directory
            walk_dir(subdir.path(), strip_components, insert_stmt, inserted_set)?;
        } else if file_type.is_file() {
            // Handle files, specifically .txt files.
            let file_path = subdir.path();
            if let Some(ext) = file_path.extension() {
                if ext == "txt" {
                    // Process txt file
                    if let Err(err) =
                        parse_txt(&file_path, strip_components, insert_stmt, inserted_set)
                    {
                        eprintln!("{err}");
                    }
                }
            }
        }
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    ffmpeg_next::init()?;
    ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Fatal);

    let mut conn = Connection::open_with_flags(
        args.db,
        OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    conn.execute(
        r#"CREATE TABLE IF NOT EXISTS song (
        path BLOB UNIQUE NOT NULL,
        title TEXT NOT NULL,
        artist TEXT NOT NULL,
        language TEXT,
        year INTEGER,
        duration REAL NOT NULL,
        lyrics TEXT,
        player_count INTEGER,
        cover_path BLOB,
        audio_path BLOB
    )"#,
        (),
    )?;

    let tx = conn.transaction()?;
    {
        let existing_songs: HashSet<_> = tx
            .prepare("SELECT path FROM song")?
            .query_map((), |row| {
                row.get::<_, Vec<u8>>(0)
                    .map(|bytes| PathBuf::from(OsStr::from_bytes(&bytes)))
            })?
            .collect::<Result<_, _>>()?;
        let mut new_songs = HashSet::new();

        let mut insert_stmt = tx.prepare(
            r#"INSERT INTO song (path, title, artist, language, year, duration, lyrics, player_count, cover_path, audio_path) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT (path) DO UPDATE SET title=?2, artist=?3, language=?4, year=?5, duration=?6, lyrics=?7, player_count = ?8, cover_path=?9, audio_path=?10"#)?;
        walk_dir(
            args.path,
            args.strip_components,
            &mut insert_stmt,
            &mut new_songs,
        )?;

        let added = new_songs.difference(&existing_songs).count();
        let removed: Vec<_> = existing_songs.difference(&new_songs).collect();

        let removed_count = if removed.is_empty() {
            0
        } else {
            println!("Trying to remove {} songs...", removed.len());
            let mut remove_stmt = tx.prepare("DELETE FROM song WHERE path=?1")?;
            removed
                .into_iter()
                .map(|path| remove_stmt.execute((path.as_os_str().as_bytes(),)))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum()
        };

        println!("{added} new songs, {removed_count} removed");
        println!(
            "Database now contains {} songs.",
            existing_songs.len() - removed_count + added
        );
    }
    tx.commit()?;

    Ok(())
}
