//! Choosing a debugger template for a program (this fork): `vx --debug <program> [-- args]`, and
//! the template picker when the current file's language has no debugger.
//!
//! Everything is decided by the debugger configs (`languages.toml`) and what is installed, never
//! by the name of a language or an adapter:
//!
//! - a template **takes a program** when it launches (`request = "launch"`) and one of its prompts
//!   is a file (`completion = "filename"`) that fills its `program` argument; it takes arguments
//!   when another prompt fills a list argument whole (`args = ["{1}"]`);
//! - a template **fits** a program when it takes one, the template's `takes` (`executable` or
//!   `source`, unset for either) agrees with what the program is, its prompts are exactly the
//!   program (and the arguments, when some were given), and its adapter's command is installed;
//! - a **source file** is a file of a language with a debugger: the templates of that language
//!   fit; anything else is an **executable**: the fitting templates of every language;
//! - within an adapter, the first fitting template in the config's order is the one (`binary`
//!   rather than `binary (terminal)`): the choice that is left is which debugger.

use std::path::{Path, PathBuf};

use vortex_core::syntax::config::{
    DebugAdapterConfig, DebugConfigCompletion, DebugTemplate, ProgramKind,
};
use vortex_view::Editor;

/// A template of a language's debugger.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub language: String,
    pub config: DebugAdapterConfig,
    pub template: DebugTemplate,
}

impl Candidate {
    /// `lldb-dap: binary`
    pub fn label(&self) -> String {
        format!("{}: {}", self.config.name, self.template.name)
    }
}

/// `vx --debug`: the program to launch and its arguments; neither, for the template picker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    pub program: Option<PathBuf>,
    pub args: Vec<String>,
}

/// The index of the prompt whose answer is the template's `program`, when it launches one from a
/// file.
pub fn program_prompt(template: &DebugTemplate) -> Option<usize> {
    if template.request != "launch" {
        return None;
    }
    let index = placeholder(template.args.get("program")?.as_str()?)?;
    match template.completion.get(index)? {
        DebugConfigCompletion::Advanced(prompt)
            if prompt.completion.as_deref() == Some("filename") =>
        {
            Some(index)
        }
        _ => None,
    }
}

/// The index of the prompt that fills a list argument whole (`args = ["{1}"]`): the program's
/// arguments.
pub fn arguments_prompt(template: &DebugTemplate) -> Option<usize> {
    template.args.values().find_map(|value| {
        value
            .as_array()?
            .iter()
            .find_map(|item| placeholder(item.as_str()?))
    })
}

/// `{N}` -> N.
fn placeholder(text: &str) -> Option<usize> {
    text.strip_prefix('{')?.strip_suffix('}')?.parse().ok()
}

/// Whether `template` launches `kind` of program with arguments or without: see the module.
pub fn fits(template: &DebugTemplate, kind: ProgramKind, with_args: bool) -> bool {
    if program_prompt(template).is_none() || template.takes.is_some_and(|takes| takes != kind) {
        return false;
    }
    match (with_args, arguments_prompt(template)) {
        (false, _) => template.completion.len() == 1,
        (true, Some(_)) => template.completion.len() == 2,
        (true, None) => false,
    }
}

/// The answers to `template`'s prompts for `program` and `args`.
pub fn params(template: &DebugTemplate, program: &Path, args: &[String]) -> Vec<String> {
    let mut params = vec![String::new(); template.completion.len()];
    if let Some(index) = program_prompt(template) {
        params[index] = program.to_string_lossy().into_owned();
    }
    if let Some(index) = arguments_prompt(template).filter(|index| *index < params.len()) {
        params[index] = command_line(args);
    }
    params
}

/// Arguments as a command line, quoted where they have spaces or quotes (the template's list
/// argument splits it back).
fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty() || arg.contains([' ', '\t', '"', '\'']) {
                format!("\"{}\"", arg.replace('"', "\\\""))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the adapter's command can run here.
fn installed(config: &DebugAdapterConfig) -> bool {
    config.command.is_empty() || vortex_stdx::env::binary_exists(&config.command)
}

/// Every language's debugger templates, in the config's order.
pub fn all(editor: &Editor) -> Vec<Candidate> {
    let loader = editor.syn_loader.load();
    loader
        .language_configs()
        .filter_map(|language| Some((language.language_id.clone(), language.debugger.clone()?)))
        .flat_map(|(language, config)| {
            config
                .templates
                .iter()
                .map(|template| Candidate {
                    language: language.clone(),
                    config: config.clone(),
                    template: template.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The language of `program`, when it is a source file of a language with a debugger.
pub fn source_language(editor: &Editor, program: &Path) -> Option<String> {
    let loader = editor.syn_loader.load();
    let language = loader.language_for_filename(program)?;
    let config = loader.language(language).config();
    config.debugger.as_ref()?;
    Some(config.language_id.clone())
}

/// The templates that fit `program` (see the module), the first per adapter, and whether it is
/// a source file.
pub fn fitting(
    candidates: Vec<Candidate>,
    source_of: Option<&str>,
    with_args: bool,
    installed: impl Fn(&DebugAdapterConfig) -> bool,
) -> Vec<Candidate> {
    let kind = if source_of.is_some() {
        ProgramKind::Source
    } else {
        ProgramKind::Executable
    };
    let mut out: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if source_of.is_some_and(|language| candidate.language != language)
            || !fits(&candidate.template, kind, with_args)
            || !installed(&candidate.config)
            || out
                .iter()
                .any(|kept| kept.config.name == candidate.config.name)
        {
            continue;
        }
        out.push(candidate);
    }
    out
}

/// `vx --debug [program] [-- args]`, once the editor is up: debug mode is on; launches the one
/// fitting template, or opens the picker of the fitting ones (of every template, without a
/// program).
pub fn start_cli(
    editor: &mut Editor,
    target: Target,
) -> Option<Box<dyn crate::compositor::Component>> {
    super::mode::enter(editor);
    let Some(program) = target.program else {
        return Some(picker(all(editor), None));
    };
    let program = vortex_stdx::path::canonicalize(&program);
    if !program.exists() {
        editor.set_error(format!("{} doesn't exist", program.display()));
        return None;
    }
    let language = source_language(editor, &program);
    let with_args = !target.args.is_empty();
    let fitting = fitting(all(editor), language.as_deref(), with_args, installed);
    match fitting.as_slice() {
        [] => {
            editor.set_error(match &language {
                Some(language) => format!(
                    "no installed debugger of {language} launches a source file{}",
                    if with_args { " with arguments" } else { "" }
                ),
                None => format!(
                    "no installed debugger launches an executable{}",
                    if with_args { " with arguments" } else { "" }
                ),
            });
            None
        }
        [one] => {
            let params = params(&one.template, &program, &target.args);
            if let Err(err) =
                crate::commands::dap::start_with(editor, &one.config, &one.template, &params)
            {
                editor.set_error(err.to_string());
            }
            None
        }
        _ => Some(picker(fitting, Some((program, target.args)))),
    }
}

/// The picker of `candidates`: each launches with `program` and its arguments filled in, or asks
/// for its prompts.
pub fn picker(
    candidates: Vec<Candidate>,
    program: Option<(PathBuf, Vec<String>)>,
) -> Box<dyn crate::compositor::Component> {
    use crate::ui::{overlay::overlaid, Picker, PickerColumn};
    let columns = [
        PickerColumn::new("template", |candidate: &Candidate, _| {
            candidate.label().into()
        }),
        PickerColumn::new("language", |candidate: &Candidate, _| {
            candidate.language.as_str().into()
        }),
    ];
    let picker =
        Picker::new(
            columns,
            0,
            candidates,
            (),
            move |cx, candidate, _action| match &program {
                Some((program, args)) => {
                    let params = params(&candidate.template, program, args);
                    if let Err(err) = crate::commands::dap::start_with(
                        cx.editor,
                        &candidate.config,
                        &candidate.template,
                        &params,
                    ) {
                        cx.editor.set_error(err.to_string());
                    }
                }
                None => crate::commands::dap::launch_template(
                    cx,
                    candidate.config.clone(),
                    candidate.template.clone(),
                ),
            },
        );
    Box::new(overlaid(picker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(toml: &str) -> DebugTemplate {
        toml::from_str(toml).unwrap()
    }

    fn adapter(name: &str, templates: Vec<DebugTemplate>) -> DebugAdapterConfig {
        let mut config: DebugAdapterConfig = toml::from_str(&format!(
            "name = '{name}'\ntransport = 'stdio'\ncommand = '{name}'\ntemplates = []"
        ))
        .unwrap();
        config.templates = templates;
        config
    }

    fn binary() -> DebugTemplate {
        template(
            r#"name = "binary"
request = "launch"
takes = "executable"
completion = [ { name = "binary", completion = "filename" } ]
args = { program = "{0}" }"#,
        )
    }

    fn binary_with_args() -> DebugTemplate {
        template(
            r#"name = "binary with arguments"
request = "launch"
takes = "executable"
completion = [ { name = "binary", completion = "filename" }, { name = "arguments", default = "" } ]
args = { program = "{0}", args = [ "{1}" ] }"#,
        )
    }

    fn terminal() -> DebugTemplate {
        template(
            r#"name = "binary (terminal)"
request = "launch"
takes = "executable"
completion = [ { name = "binary", completion = "filename" } ]
args = { program = "{0}", runInTerminal = true }"#,
        )
    }

    fn attach() -> DebugTemplate {
        template(
            r#"name = "attach"
request = "attach"
completion = [ "pid" ]
args = { pid = "{0}" }"#,
        )
    }

    fn source() -> DebugTemplate {
        template(
            r#"name = "source"
request = "launch"
takes = "source"
completion = [ { name = "main", completion = "filename", default = "index.js" } ]
args = { type = "pwa-node", program = "{0}" }"#,
        )
    }

    fn candidates() -> Vec<Candidate> {
        let lldb = adapter(
            "lldb",
            vec![binary(), binary_with_args(), terminal(), attach()],
        );
        let js = adapter("js", vec![source(), attach()]);
        let mut out = Vec::new();
        for (language, config) in [("rust", &lldb), ("c", &lldb), ("javascript", &js)] {
            for template in &config.templates {
                out.push(Candidate {
                    language: language.into(),
                    config: config.clone(),
                    template: template.clone(),
                });
            }
        }
        out
    }

    fn labels(candidates: &[Candidate]) -> Vec<String> {
        candidates.iter().map(Candidate::label).collect()
    }

    #[test]
    fn what_takes_a_program() {
        assert_eq!(program_prompt(&binary()), Some(0));
        assert_eq!(program_prompt(&attach()), None);
        assert_eq!(arguments_prompt(&binary_with_args()), Some(1));
        assert_eq!(arguments_prompt(&binary()), None);
        assert!(fits(&binary(), ProgramKind::Executable, false));
        assert!(!fits(&binary(), ProgramKind::Executable, true));
        assert!(fits(&binary_with_args(), ProgramKind::Executable, true));
        assert!(!fits(&binary_with_args(), ProgramKind::Executable, false));
        assert!(!fits(&binary(), ProgramKind::Source, false));
        assert!(fits(&source(), ProgramKind::Source, false));
        assert!(!fits(&source(), ProgramKind::Executable, false));
        assert!(!fits(&attach(), ProgramKind::Executable, false));
    }

    #[test]
    fn an_executable_gets_the_first_fitting_template_of_each_adapter() {
        let all = |_: &DebugAdapterConfig| true;
        let fit = fitting(candidates(), None, false, all);
        assert_eq!(labels(&fit), ["lldb: binary"]);
        let fit = fitting(candidates(), None, true, all);
        assert_eq!(labels(&fit), ["lldb: binary with arguments"]);
    }

    #[test]
    fn a_source_file_gets_its_language_templates() {
        let all = |_: &DebugAdapterConfig| true;
        let fit = fitting(candidates(), Some("javascript"), false, all);
        assert_eq!(labels(&fit), ["js: source"]);
        // no template of rust launches a source file
        assert!(fitting(candidates(), Some("rust"), false, all).is_empty());
    }

    #[test]
    fn several_adapters_fit_and_uninstalled_ones_dont() {
        let mut candidates = candidates();
        let gdb = adapter("gdb", vec![binary()]);
        candidates.push(Candidate {
            language: "c".into(),
            config: gdb,
            template: binary(),
        });
        let fit = fitting(candidates.clone(), None, false, |_| true);
        assert_eq!(labels(&fit), ["lldb: binary", "gdb: binary"]);
        let fit = fitting(candidates, None, false, |config| config.name == "gdb");
        assert_eq!(labels(&fit), ["gdb: binary"]);
    }

    #[test]
    fn params_fill_the_program_and_the_arguments() {
        let program = Path::new("/tmp/app");
        assert_eq!(params(&binary(), program, &[]), ["/tmp/app"]);
        let args = ["--port".to_string(), "8080".into(), "a b".into()];
        assert_eq!(
            params(&binary_with_args(), program, &args),
            ["/tmp/app", "--port 8080 \"a b\""]
        );
    }
}
