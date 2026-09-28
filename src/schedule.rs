use std::fs;
use std::path::Path;
use std::process::Command;

use jiff::Zoned;

use crate::error::AppError;

const TASK: &str = "trail-scan";

/// Registers the scheduled scan. Windows gets a Task Scheduler task; elsewhere the crontab
/// line to add is returned. `scratch` holds the task XML while schtasks reads it.
pub fn install(exe: &Path, scratch: &Path) -> Result<String, AppError> {
    let exe = exe.display().to_string();
    if !cfg!(windows) {
        return Ok(format!(
            "Add this line with `crontab -e`:\n{}\n",
            crontab(&exe)
        ));
    }
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (Err(_), Ok(name)) => name,
        _ => return Err(AppError::Schedule("USERNAME is not set".into())),
    };
    let start = Zoned::now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let xml = scratch.join("trail-task.xml");
    fs::write(&xml, utf16le(&task_xml(&exe, &user, &start))).map_err(|source| AppError::Write {
        path: xml.clone(),
        source,
    })?;
    let created = schtasks(&[
        "/create",
        "/tn",
        TASK,
        "/xml",
        &xml.display().to_string(),
        "/f",
    ]);
    // Best effort: a leftover XML file in the state dir is harmless.
    fs::remove_file(&xml).ok();
    created?;
    Ok(format!(
        "Scheduled task `{TASK}` runs `trail scan --quiet` at logon and every 30 minutes.\n"
    ))
}

pub fn uninstall() -> Result<String, AppError> {
    if !cfg!(windows) {
        return Ok("Remove the `trail scan` line with `crontab -e`.\n".into());
    }
    schtasks(&["/delete", "/tn", TASK, "/f"])?;
    Ok(format!("Removed scheduled task `{TASK}`.\n"))
}

fn schtasks(args: &[&str]) -> Result<(), AppError> {
    let out = Command::new("schtasks")
        .args(args)
        .output()
        .map_err(|e| AppError::Schedule(format!("cannot run schtasks: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Schedule(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

/// Task Scheduler definition: logon trigger plus a 30 minute repetition, run through
/// `conhost --headless` so no console window opens.
#[must_use]
fn task_xml(exe: &str, user: &str, start: &str) -> String {
    let (exe, user) = (escape(exe), escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>trail: record every local git commit</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
    <TimeTrigger>
      <Repetition>
        <Interval>PT30M</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT15M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>conhost.exe</Command>
      <Arguments>--headless "{exe}" scan --quiet</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[must_use]
fn crontab(exe: &str) -> String {
    format!("*/30 * * * * '{exe}' scan --quiet")
}

/// schtasks only reads task XML reliably as UTF-16 with a byte order mark.
#[must_use]
fn utf16le(s: &str) -> Vec<u8> {
    [0xFF, 0xFE]
        .into_iter()
        .chain(s.encode_utf16().flat_map(u16::to_le_bytes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_runs_headless_scan_at_logon_and_every_30_minutes() {
        let xml = task_xml(
            r"C:\Tools & Co\trail.exe",
            r"DOMAIN\bk",
            "2026-09-25T15:00:00",
        );
        insta::assert_snapshot!(xml, @r#"
        <?xml version="1.0" encoding="UTF-16"?>
        <Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
          <RegistrationInfo>
            <Description>trail: record every local git commit</Description>
          </RegistrationInfo>
          <Triggers>
            <LogonTrigger>
              <Enabled>true</Enabled>
              <UserId>DOMAIN\bk</UserId>
            </LogonTrigger>
            <TimeTrigger>
              <Repetition>
                <Interval>PT30M</Interval>
                <StopAtDurationEnd>false</StopAtDurationEnd>
              </Repetition>
              <StartBoundary>2026-09-25T15:00:00</StartBoundary>
              <Enabled>true</Enabled>
            </TimeTrigger>
          </Triggers>
          <Principals>
            <Principal id="Author">
              <UserId>DOMAIN\bk</UserId>
              <LogonType>InteractiveToken</LogonType>
              <RunLevel>LeastPrivilege</RunLevel>
            </Principal>
          </Principals>
          <Settings>
            <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
            <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
            <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
            <StartWhenAvailable>true</StartWhenAvailable>
            <Enabled>true</Enabled>
            <ExecutionTimeLimit>PT15M</ExecutionTimeLimit>
          </Settings>
          <Actions Context="Author">
            <Exec>
              <Command>conhost.exe</Command>
              <Arguments>--headless "C:\Tools &amp; Co\trail.exe" scan --quiet</Arguments>
            </Exec>
          </Actions>
        </Task>
        "#);
    }

    #[test]
    fn crontab_line_quotes_the_binary() {
        assert_eq!(
            crontab("/home/bk/.cargo/bin/trail"),
            "*/30 * * * * '/home/bk/.cargo/bin/trail' scan --quiet"
        );
    }

    #[test]
    fn utf16_has_bom_and_little_endian_units() {
        assert_eq!(utf16le("<ø"), [0xFF, 0xFE, b'<', 0, 0xF8, 0]);
    }
}
