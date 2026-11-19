//! Questions for the input a command needs and was not given.
//!
//! Every answer a command asks for is also a flag. A command collects its
//! input through one [`Form`]: a value given as a flag is used as is, a
//! missing one is asked for on a terminal, and without a terminal (stdin or
//! stderr is not one, or `--no-input` is set) the form records the flag and
//! [`Form::finish`] fails naming every missing flag at once, so a script never
//! waits on a question. A command given all its required input asks nothing.
//! Questions, explanations, and summaries go to stderr,
//! so `--format json` keeps stdout for the result.

use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicBool, Ordering};

use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password, Select};
use miette::IntoDiagnostic;
use owo_colors::OwoColorize as _;

static NO_INPUT: AtomicBool = AtomicBool::new(false);

/// Record whether `--no-input` was given for this process.
pub fn init(no_input: bool) {
    NO_INPUT.store(no_input, Ordering::Relaxed);
}

/// Whether `--no-input` was given.
pub fn no_input() -> bool {
    NO_INPUT.load(Ordering::Relaxed)
}

/// Whether a person can answer questions: both stdin and stderr are
/// terminals and `--no-input` was not given.
pub fn interactive() -> bool {
    !no_input() && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// One command's input, asked for where a flag left it out.
pub struct Form {
    interactive: bool,
    asked: bool,
    missing: Vec<String>,
    theme: ColorfulTheme,
}

impl Form {
    /// A form that asks questions when `asking` (the command lacks required
    /// input) and [`interactive`] both hold. A command given everything it
    /// requires asks nothing, not even its optional questions, and runs as a
    /// script would.
    pub fn new(asking: bool) -> Self {
        Self {
            interactive: asking && interactive(),
            asked: false,
            missing: Vec::new(),
            theme: ColorfulTheme::default(),
        }
    }

    /// Whether this form asks questions.
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Explain the next question, on a terminal only.
    pub fn explain(&self, text: &str) {
        if self.interactive {
            anstream::eprintln!("{}", text.dimmed());
        }
    }

    /// A required text value: `given`, else asked for (starting from
    /// `current`), else `current`, else recorded as missing under `flag`.
    pub fn required(
        &mut self,
        question: &str,
        flag: &str,
        given: Option<String>,
        current: Option<String>,
        check: impl Fn(&str) -> Result<(), String>,
    ) -> miette::Result<String> {
        if let Some(given) = given {
            return Ok(given);
        }
        if !self.interactive {
            return Ok(current.unwrap_or_else(|| {
                self.missing.push(flag.to_owned());
                String::new()
            }));
        }
        self.asked = true;
        let input = Input::<String>::with_theme(&self.theme)
            .with_prompt(question)
            .with_initial_text(current.unwrap_or_default())
            .validate_with(|answer: &String| {
                if answer.trim().is_empty() {
                    return Err("An answer is needed.".to_owned());
                }
                check(answer.trim())
            });
        Ok(input.interact_text().into_diagnostic()?.trim().to_owned())
    }

    /// An optional text value: `given`, else asked for (starting from
    /// `current`, which an empty answer clears), else `current`.
    pub fn optional(
        &mut self,
        question: &str,
        given: Option<String>,
        current: String,
        check: impl Fn(&str) -> Result<(), String>,
    ) -> miette::Result<String> {
        if let Some(given) = given {
            return Ok(given);
        }
        if !self.interactive {
            return Ok(current);
        }
        self.asked = true;
        let input = Input::<String>::with_theme(&self.theme)
            .with_prompt(question)
            .with_initial_text(current)
            .allow_empty(true)
            .validate_with(|answer: &String| {
                if answer.trim().is_empty() {
                    Ok(())
                } else {
                    check(answer.trim())
                }
            });
        Ok(input.interact_text().into_diagnostic()?.trim().to_owned())
    }

    /// One of a known set of `choices`, each with a plain-language label:
    /// `given`, else asked for (starting at `current`), else `current`, else
    /// recorded as missing under `flag`.
    pub fn choose<T: Clone>(
        &mut self,
        question: &str,
        flag: &str,
        given: Option<T>,
        choices: &[(T, String)],
        current: Option<usize>,
    ) -> miette::Result<T> {
        if let Some(given) = given {
            return Ok(given);
        }
        if !self.interactive {
            return Ok(match current {
                Some(index) => choices[index].0.clone(),
                None => {
                    self.missing.push(flag.to_owned());
                    choices[0].0.clone()
                }
            });
        }
        self.asked = true;
        let index = Select::with_theme(&self.theme)
            .with_prompt(question)
            .items(choices.iter().map(|(_, label)| label))
            .default(current.unwrap_or(0))
            .interact()
            .into_diagnostic()?;
        Ok(choices[index].0.clone())
    }

    /// A yes-or-no setting: `given`, else asked for, else `current`.
    pub fn yes_no(
        &mut self,
        question: &str,
        given: Option<bool>,
        current: bool,
    ) -> miette::Result<bool> {
        if let Some(given) = given {
            return Ok(given);
        }
        if !self.interactive {
            return Ok(current);
        }
        self.asked = true;
        Confirm::with_theme(&self.theme)
            .with_prompt(question)
            .default(current)
            .interact()
            .into_diagnostic()
    }

    /// Several values on one line, separated by spaces, each checked as it is
    /// typed: `given` when any, else asked for (starting from `current`), else
    /// `current`. A `required` list that ends up empty is recorded as missing
    /// under `flag`.
    pub fn list(
        &mut self,
        question: &str,
        flag: &str,
        given: Vec<String>,
        current: Vec<String>,
        required: bool,
        check: impl Fn(&str) -> Result<(), String>,
    ) -> miette::Result<Vec<String>> {
        if !given.is_empty() {
            return Ok(given);
        }
        if !self.interactive {
            if required && current.is_empty() {
                self.missing.push(flag.to_owned());
            }
            return Ok(current);
        }
        self.asked = true;
        let input = Input::<String>::with_theme(&self.theme)
            .with_prompt(question)
            .with_initial_text(current.join(" "))
            .allow_empty(!required)
            .validate_with(|answer: &String| {
                if required && answer.trim().is_empty() {
                    return Err("At least one is needed.".to_owned());
                }
                answer.split_whitespace().try_for_each(&check)
            });
        Ok(input
            .interact_text()
            .into_diagnostic()?
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    }

    /// An acknowledgement a person must give, such as accepting a breaking
    /// change: `given` when set, else asked for (an answer of no returns
    /// false), else recorded as missing under `flag`.
    pub fn acknowledge(&mut self, question: &str, flag: &str, given: bool) -> miette::Result<bool> {
        if given {
            return Ok(true);
        }
        if !self.interactive {
            self.missing.push(flag.to_owned());
            return Ok(false);
        }
        self.asked = true;
        Confirm::with_theme(&self.theme)
            .with_prompt(question)
            .default(false)
            .interact()
            .into_diagnostic()
    }

    /// An optional secret, typed without echo: `given`, else asked for (an
    /// empty answer is none), else none.
    pub fn secret(
        &mut self,
        question: &str,
        given: Option<String>,
    ) -> miette::Result<Option<String>> {
        if given.is_some() || !self.interactive {
            return Ok(given);
        }
        self.asked = true;
        let secret = Password::with_theme(&self.theme)
            .with_prompt(question)
            .allow_empty_password(true)
            .interact()
            .into_diagnostic()?;
        Ok((!secret.is_empty()).then_some(secret))
    }

    /// A required secret, typed without echo and then again to confirm,
    /// else recorded as missing under `flag`.
    pub fn required_secret(&mut self, question: &str, flag: &str) -> miette::Result<String> {
        if !self.interactive {
            self.missing.push(flag.to_owned());
            return Ok(String::new());
        }
        self.asked = true;
        Password::with_theme(&self.theme)
            .with_prompt(question)
            .with_confirmation("Type it again", "The two values don't match.")
            .interact()
            .into_diagnostic()
    }

    /// Fail, naming every flag still missing, when the form could not ask.
    pub fn finish(&self) -> miette::Result<()> {
        if self.missing.is_empty() {
            return Ok(());
        }
        Err(miette::miette!(
            help = if no_input() {
                "Give them as flags; --no-input turns questions off."
            } else {
                "Give them as flags, or run the command in a terminal to be asked."
            },
            "missing required input: {}",
            self.missing.join(", ")
        ))
    }

    /// After questions were asked, show what will be saved and ask to go
    /// ahead. A run that asked nothing, such as one given every flag, goes
    /// ahead as before.
    pub fn confirm(&self, title: &str, summary: &[(&str, String)]) -> miette::Result<()> {
        if !self.asked {
            return Ok(());
        }
        let width = summary
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        anstream::eprintln!();
        anstream::eprintln!("{}", title.bold());
        for (label, value) in summary {
            let value = if value.is_empty() {
                "none"
            } else {
                value.as_str()
            };
            anstream::eprintln!("  {}  {value}", format!("{label:<width$}").dimmed());
        }
        let go = Confirm::with_theme(&self.theme)
            .with_prompt("Go ahead?")
            .default(true)
            .interact()
            .into_diagnostic()?;
        miette::ensure!(go, "cancelled; nothing changed");
        Ok(())
    }
}

/// `--flag`/`--no-flag` as a setting: `Some` when either was given.
pub fn pair(on: bool, off: bool) -> Option<bool> {
    (on || off).then_some(on)
}
