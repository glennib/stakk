//! Command execution for `jj`.

use crate::jj::JjError;

/// Trait for running `jj` commands. Abstracted for testing.
pub trait JjRunner: Send + Sync {
    fn run_jj(
        &self,
        args: &[&str],
    ) -> impl std::future::Future<Output = Result<String, JjError>> + Send;
}

/// Runs `jj` commands via `tokio::process::Command`.
pub struct RealJjRunner;

impl JjRunner for RealJjRunner {
    async fn run_jj(&self, args: &[&str]) -> Result<String, JjError> {
        let output = tokio::process::Command::new("jj")
            .arg("--config")
            .arg("ui.paginate=never")
            .args(args)
            .output()
            .await
            .map_err(JjError::NotFound)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(JjError::CommandFailed {
                command: render_command(args),
                stderr,
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Render the full jj invocation (including the always-passed `--config`
/// prefix) as a copy-pasteable shell-style string for error messages.
fn render_command(args: &[&str]) -> String {
    std::iter::once("jj")
        .chain(["--config", "ui.paginate=never"])
        .chain(args.iter().copied())
        .map(|arg| {
            if arg.is_empty() || arg.contains(char::is_whitespace) {
                format!("'{}'", arg.replace('\'', r"'\''"))
            } else {
                arg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_command_includes_config_prefix() {
        let rendered = render_command(&["git", "push", "--remote", "origin"]);
        assert_eq!(
            rendered,
            "jj --config ui.paginate=never git push --remote origin"
        );
    }

    #[test]
    fn render_command_quotes_args_with_whitespace() {
        let rendered = render_command(&["log", "-T", r#"json(self) ++ "\n""#]);
        assert_eq!(
            rendered,
            r#"jj --config ui.paginate=never log -T 'json(self) ++ "\n"'"#
        );
    }
}
