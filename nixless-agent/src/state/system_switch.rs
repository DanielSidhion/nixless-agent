use std::path::PathBuf;

use anyhow::anyhow;

use crate::path_utils::remove_file_with_check;

pub enum SystemSwitchStatus {
    Successful { reboot_required: bool },
    Failed(SwitchStatusCodes),
    InProgress,
}

pub struct SwitchStatusCodes {
    pub service_result: String,
    pub exit_code: String,
    pub exit_status: String,
}

/// Will also clean up the tracking files if they exist.
pub async fn check_switching_status(directory: &PathBuf) -> anyhow::Result<SystemSwitchStatus> {
    let started_path = directory.join("nixless-agent/pre_switch");
    let success_path = directory.join("nixless-agent/switch_success");
    let finish_path = directory.join("nixless-agent/post_switch");

    let finished = finish_path.try_exists()?;
    let started = started_path.try_exists()?;
    let successful = success_path.try_exists()?;

    match (started, finished, successful) {
        (true, true, true) => {
            clean_up_system_switch_tracking_files(directory).await?;
            Ok(SystemSwitchStatus::Successful {
                reboot_required: false,
            })
        }
        (true, true, false) => {
            let status_code_contents = tokio::fs::read_to_string(finish_path).await?;
            let [service_result, exit_code, exit_status] =
                status_code_contents.lines().collect::<Vec<_>>()[..]
            else {
                return Err(anyhow!(
                    "the tracking file for finished status didn't follow the expected format"
                ));
            };

            clean_up_system_switch_tracking_files(directory).await?;

            if service_result == "exit-code" && exit_status == "100" {
                Ok(SystemSwitchStatus::Successful {
                    reboot_required: true,
                })
            } else {
                let status_codes = SwitchStatusCodes {
                    service_result: service_result.to_string(),
                    exit_code: exit_code.to_string(),
                    exit_status: exit_status.to_string(),
                };

                Ok(SystemSwitchStatus::Failed(status_codes))
            }
        }
        (_, false, _) | (false, _, _) => Ok(SystemSwitchStatus::InProgress),
    }
}

async fn clean_up_system_switch_tracking_files(directory: &PathBuf) -> anyhow::Result<()> {
    let started_path = directory.join("nixless-agent/pre_switch");
    let success_path = directory.join("nixless-agent/switch_success");
    let finish_path = directory.join("nixless-agent/post_switch");

    let (r1, r2, r3) = tokio::join!(
        remove_file_with_check(started_path),
        remove_file_with_check(success_path),
        remove_file_with_check(finish_path)
    );
    r1?;
    r2?;
    r3?;

    Ok(())
}
