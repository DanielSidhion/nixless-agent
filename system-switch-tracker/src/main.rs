use std::{
    env,
    fs::File,
    os::unix::fs::{chown, OpenOptionsExt},
    path::PathBuf,
    process::exit,
    str::FromStr,
};

fn get_user_group_id(user_name: &str) -> std::io::Result<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let user_entry = passwd.lines().find(|line| line.starts_with(user_name));

    if let Some(user_entry) = user_entry {
        let [_, _, uid, gid, _] = user_entry.splitn(5, ":").collect::<Vec<_>>()[..] else {
            return Err(std::io::Error::other(
                "passwd line didn't follow expected format",
            ));
        };
        let uid = uid
            .parse()
            .expect("passwd line didn't follow expected format");
        let gid = gid
            .parse()
            .expect("passwd line didn't follow expected format");
        Ok((uid, gid))
    } else {
        Err(std::io::Error::other("user not found"))
    }
}

fn main() {
    let args: Vec<_> = env::args().collect();

    let [_, track_mode, track_directory_path, agent_user] = &args[..] else {
        eprintln!(
            "Received wrong number of arguments, was expecting {}, got {}.",
            4,
            args.len()
        );
        exit(1);
    };

    let track_file_name = match track_mode.as_str() {
        "pre-switch" => "pre_switch",
        "switch-success" => "switch_success",
        "post-switch" => "post_switch",
        _ => {
            eprintln!(
                "Expected the track mode to be one of 'pre-switch', 'switch-success', 'post-switch', but got '{}'.",
                track_mode
            );
            exit(1);
        }
    };

    let track_directory_path = PathBuf::from_str(&track_directory_path)
        .expect("the directory to keep the tracking files can't be read as a path");

    let (user_id, group_id) = get_user_group_id(agent_user)
        .expect("failed to retrieve id of user associated with given user name");

    let file_path = track_directory_path.join(track_file_name);
    let file = File::options()
        .mode(0o600)
        .write(true)
        .create_new(true)
        .open(&file_path)
        .expect("couldn't create a new tracking file");

    drop(file);

    chown(file_path, Some(user_id), Some(group_id))
        .expect("failed to set proper owner for the tracking file");
}
