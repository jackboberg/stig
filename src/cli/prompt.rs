use crate::errors::CliError;

pub(crate) fn confirm_or_abort(yes: bool, prompt: &str) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    match dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(false)
        .interact()
    {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(CliError::Declined.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_flag_skips_prompt() {
        assert!(confirm_or_abort(true, "test prompt").is_ok());
    }

    #[test]
    fn non_tty_returns_declined() {
        let result = confirm_or_abort(false, "test prompt");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<CliError>().is_some());
        match err.downcast_ref::<CliError>().unwrap() {
            CliError::Declined => (),
            _ => panic!("expected CliError::Declined"),
        }
    }
}
