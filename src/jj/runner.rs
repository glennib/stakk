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
            return Err(JjError::CommandFailed { stderr });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
