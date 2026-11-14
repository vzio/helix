use anyhow::Result;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use vortex_core::Position;
use vortex_view::tree::Layout;

#[derive(Default)]
pub struct Args {
    pub display_help: bool,
    pub display_version: bool,
    pub health: bool,
    pub health_arg: Option<String>,
    pub load_tutor: bool,
    pub fetch_grammars: bool,
    pub build_grammars: bool,
    pub strict: bool,
    pub split: Option<Layout>,
    pub verbosity: u64,
    pub log_file: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub files: IndexMap<PathBuf, Vec<Position>>,
    pub working_directory: Option<PathBuf>,
    /// `--watch [folder]`: start in watch mode (fork feature).
    pub watch: bool,
    /// `--plain`, `-p`: start without the dashboard, whatever the config says (fork feature).
    pub plain: bool,
    /// `--debug [program] [-- args]`: start in debug mode and launch the program (fork feature).
    pub debug: Option<crate::debug::launch::Target>,
}

impl Args {
    /// The directory to start in: the `-w` flag wins, else the first file when it is a
    /// directory (`vx <folder>`), else none: the process keeps its own.
    pub fn initial_working_dir(&self) -> Option<&Path> {
        self.working_directory.as_deref().or_else(|| {
            self.files
                .first()
                .map(|(path, _)| path.as_path())
                .filter(|path| path.is_dir())
        })
    }

    pub fn parse_args() -> Result<Args> {
        Self::parse_from(std::env::args())
    }

    /// [`Self::parse_args`] of the command line `argv`, program name first.
    #[allow(clippy::too_many_lines)]
    pub fn parse_from(argv: impl IntoIterator<Item = String>) -> Result<Args> {
        let mut args = Args::default();
        let mut argv = argv.into_iter().peekable();
        let mut line_number = 0;

        let mut insert_file_with_position = |file_with_position: &str| {
            let (filename, position) = parse_file(file_with_position);

            // Before setting the working directory, resolve all the paths in args.files
            let filename = vortex_stdx::path::canonicalize(filename);

            args.files
                .entry(filename)
                .and_modify(|positions| positions.push(position))
                .or_insert_with(|| vec![position]);
        };

        argv.next(); // skip the program, we don't care about that

        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "--" => break, // stop parsing at this point treat the remaining as files
                "--version" => args.display_version = true,
                "--help" => args.display_help = true,
                "--strict" => args.strict = true,
                "--tutor" => args.load_tutor = true,
                "--vsplit" => match args.split {
                    Some(_) => anyhow::bail!("can only set a split once of a specific type"),
                    None => args.split = Some(Layout::Vertical),
                },
                "--hsplit" => match args.split {
                    Some(_) => anyhow::bail!("can only set a split once of a specific type"),
                    None => args.split = Some(Layout::Horizontal),
                },
                "--health" => {
                    args.health = true;
                    args.health_arg = argv.next_if(|opt| !opt.starts_with('-'));
                }
                "-g" | "--grammar" => match argv.next().as_deref() {
                    Some("fetch") => args.fetch_grammars = true,
                    Some("build") => args.build_grammars = true,
                    _ => {
                        anyhow::bail!("--grammar must be followed by either 'fetch' or 'build'")
                    }
                },
                "-c" | "--config" => match argv.next().as_deref() {
                    Some(path) => args.config_file = Some(path.into()),
                    None => anyhow::bail!("--config must specify a path to read"),
                },
                "--log" => match argv.next().as_deref() {
                    Some(path) => args.log_file = Some(path.into()),
                    None => anyhow::bail!("--log must specify a path to write"),
                },
                "-w" | "--working-dir" => match argv.next().as_deref() {
                    Some(path) => {
                        args.working_directory = if Path::new(path).is_dir() {
                            Some(PathBuf::from(path))
                        } else {
                            anyhow::bail!(
                                "--working-dir specified does not exist or is not a directory"
                            )
                        }
                    }
                    None => {
                        anyhow::bail!("--working-dir must specify an initial working directory")
                    }
                },
                // The terminal panel's `$EDITOR` is `vx --wait`, which waits there until the
                // buffer is closed; outside the panel an editor always waits for itself.
                "--wait" => {}
                "--plain" => args.plain = true,
                // `--debug [program] [-- args]`: debug mode, launching the program; everything
                // after `--` is its arguments
                "--debug" => {
                    let program = argv
                        .next_if(|arg| !arg.starts_with('-'))
                        .map(|program| vortex_stdx::path::canonicalize(PathBuf::from(program)));
                    let mut program_args = Vec::new();
                    if program.is_some() && argv.next_if(|arg| arg == "--").is_some() {
                        program_args.extend(argv.by_ref());
                    }
                    args.debug = Some(crate::debug::launch::Target {
                        program,
                        args: program_args,
                    });
                }
                // `--watch [folder]`: watch mode, in the folder when one follows
                "--watch" => {
                    args.watch = true;
                    if let Some(folder) =
                        argv.next_if(|arg| !arg.starts_with('-') && Path::new(arg).is_dir())
                    {
                        args.working_directory = Some(vortex_stdx::path::canonicalize(folder));
                    }
                }
                arg if arg.starts_with("--") => {
                    anyhow::bail!("unexpected double dash argument: {}", arg)
                }
                arg if arg.starts_with('-') => {
                    let arg = arg.get(1..).unwrap().chars();
                    for chr in arg {
                        match chr {
                            'v' => args.verbosity += 1,
                            'V' => args.display_version = true,
                            'h' => args.display_help = true,
                            'p' => args.plain = true,
                            _ => anyhow::bail!("unexpected short arg {}", chr),
                        }
                    }
                }
                "+" => line_number = usize::MAX,
                arg if arg.starts_with('+') => {
                    match arg[1..].parse::<usize>() {
                        Ok(n) => line_number = n.saturating_sub(1),
                        _ => insert_file_with_position(arg),
                    };
                }
                arg => insert_file_with_position(arg),
            }
        }

        // push the remaining args, if any to the files
        for arg in argv {
            insert_file_with_position(&arg);
        }

        if line_number != 0 {
            if let Some(first_position) = args
                .files
                .first_mut()
                .and_then(|(_, positions)| positions.first_mut())
            {
                first_position.row = line_number;
            }
        }

        Ok(args)
    }
}

/// Parse arg into [`PathBuf`] and position.
pub(crate) fn parse_file(s: &str) -> (PathBuf, Position) {
    let def = || (PathBuf::from(s), Position::default());
    if Path::new(s).exists() {
        return def();
    }
    split_path_row_col(s)
        .or_else(|| split_path_row(s))
        .unwrap_or_else(def)
}

/// Split file.rs:10:2 into [`PathBuf`], row and col.
///
/// Does not validate if file.rs is a file or directory.
fn split_path_row_col(s: &str) -> Option<(PathBuf, Position)> {
    let mut s = s.trim_end_matches(':').rsplitn(3, ':');
    let col: usize = s.next()?.parse().ok()?;
    let row: usize = s.next()?.parse().ok()?;
    let path = s.next()?.into();
    let pos = Position::new(row.saturating_sub(1), col.saturating_sub(1));
    Some((path, pos))
}

/// Split file.rs:10 into [`PathBuf`] and row.
///
/// Does not validate if file.rs is a file or directory.
fn split_path_row(s: &str) -> Option<(PathBuf, Position)> {
    let (path, row) = s.trim_end_matches(':').rsplit_once(':')?;
    let row: usize = row.parse().ok()?;
    let path = path.into();
    let pos = Position::new(row.saturating_sub(1), 0);
    Some((path, pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(files: &[&Path], working_directory: Option<&Path>) -> Args {
        Args {
            files: files
                .iter()
                .map(|path| (path.to_path_buf(), vec![Position::default()]))
                .collect(),
            working_directory: working_directory.map(Path::to_path_buf),
            ..Args::default()
        }
    }

    #[test]
    fn a_folder_is_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let args = args(&[dir.path()], None);
        assert_eq!(args.initial_working_dir(), Some(dir.path()));
    }

    #[test]
    fn a_file_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(args(&[&file], None).initial_working_dir(), None);
        assert_eq!(args(&[], None).initial_working_dir(), None);
    }

    #[test]
    fn only_the_first_file_counts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(args(&[&file, dir.path()], None).initial_working_dir(), None);
    }

    #[test]
    fn the_flag_wins() {
        let folder = tempfile::tempdir().unwrap();
        let flag = tempfile::tempdir().unwrap();
        let args = args(&[folder.path()], Some(flag.path()));
        assert_eq!(args.initial_working_dir(), Some(flag.path()));
    }

    fn parse(argv: &[&str]) -> Args {
        let argv = std::iter::once("vx".to_string()).chain(argv.iter().map(|arg| arg.to_string()));
        Args::parse_from(argv).unwrap()
    }

    #[test]
    fn plain_in_long_and_short_forms() {
        assert!(parse(&["--plain"]).plain);
        assert!(parse(&["-p"]).plain);
        assert!(parse(&["-vp", "file.txt"]).plain);
        assert!(!parse(&[]).plain);
        assert!(!parse(&["file.txt"]).plain);
    }

    #[test]
    fn debug_takes_a_program_and_its_arguments() {
        let args = parse(&["--debug"]);
        assert_eq!(args.debug, Some(crate::debug::launch::Target::default()));
        assert!(parse(&[]).debug.is_none());

        let args = parse(&["--debug", "target/app", "--", "--port", "8080", "-v"]);
        let target = args.debug.unwrap();
        assert!(target.program.unwrap().ends_with("target/app"));
        assert_eq!(target.args, ["--port", "8080", "-v"]);
        // the program's arguments aren't files to open
        assert!(args.files.is_empty());

        let args = parse(&["--debug", "app", "--plain"]);
        assert!(args.debug.unwrap().args.is_empty());
        assert!(args.plain);
    }

    #[test]
    fn watch_alone_just_sets_the_flag() {
        let args = parse(&["--watch"]);
        assert!(args.watch);
        assert_eq!(args.working_directory, None);
        assert!(args.files.is_empty());
        assert!(!parse(&[]).watch);
    }

    #[test]
    fn watch_takes_a_folder_as_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().to_str().unwrap();
        let args = parse(&["--watch", folder]);
        assert!(args.watch);
        assert_eq!(
            args.working_directory,
            Some(vortex_stdx::path::canonicalize(dir.path()))
        );
        // it is not opened as a file too
        assert!(args.files.is_empty());
    }

    #[test]
    fn watch_ignores_a_following_file_and_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let args = parse(&["--watch", file.to_str().unwrap()]);
        assert!(args.watch);
        assert_eq!(args.working_directory, None);
        // the file is still opened
        assert_eq!(args.files.len(), 1);

        let missing = dir.path().join("nope");
        let args = parse(&["--watch", missing.to_str().unwrap()]);
        assert_eq!(args.working_directory, None);
        assert_eq!(args.files.len(), 1);
    }

    #[test]
    fn watch_leaves_the_flags_after_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        let args = parse(&["--watch", "--vsplit", dir.path().to_str().unwrap()]);
        assert!(args.watch);
        assert_eq!(args.working_directory, None);
        assert!(args.split.is_some());
    }
}
