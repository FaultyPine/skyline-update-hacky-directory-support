use std::path::{PathBuf, Path};
use std::io::prelude::*;
use std::net::{TcpStream, IpAddr};

use update_protocol::{Request, ResponseCode};

pub use update_protocol::UpdateResponse;

const PORT: u16 = 45000;

pub struct DefaultInstaller;

#[cfg(not(target_os = "switch"))]
impl Installer for DefaultInstaller {
    fn should_update(&self, _: &UpdateResponse) -> bool {
        true
    }

    fn install_file(&self, path: PathBuf, buf: Vec<u8>) -> Result<(), ()> {
        println!("Installing {} bytes to path {}", buf.len(), path.display());

        if let Ok(string) = String::from_utf8(buf) {
            println!("As string: {:?}", string);
        }

        Ok(())
    }
}

#[cfg(target_os = "switch")]
impl Installer for DefaultInstaller {
    fn should_update(&self, response: &UpdateResponse) -> bool {

        if Path::new("sd:/installing.tmpfile").exists() {
            return true;
        }

        skyline_web::Dialog::yes_no(format!(
            "An update for {} has been found.\n\nWould you like to download it?",
            response.plugin_name
        ))
    }

    fn install_file(&self, path: PathBuf, buf: Vec<u8>) -> Result<(), ()> {
        if path.parent().ok_or(()) != Ok(Path::new("sd:")) {
            let _ = std::fs::create_dir_all(path.parent().ok_or(())?);
        }
        if let Err(e) = std::fs::write(path, buf) {
            println!("[updater] Error writing file to sd: {}", e);
            Err(())
        } else {
            Ok(())
        }
    }
}

/// An installer for use with custom_check_update
pub trait Installer {
    fn should_update(&self, response: &UpdateResponse) -> bool;
    fn install_file(&self, path: PathBuf, buf: Vec<u8>) -> Result<(), ()>;
}

fn update<I>(ip: IpAddr, response: &UpdateResponse, installer: &I) -> bool
    where I: Installer,
{

    /* Remove dir(s) before installing. This makes sure that even if you remove files in your folders it will update properly */
    if !Path::new("sd:/installing.tmpfile").exists() {
        for file in &response.required_files {
            if let update_protocol::InstallLocation::AbsolutePath(p) = &file.install_location {
                let p = Path::new(&p);
                if p.is_dir() && p.exists() {
                    println!("Deleting folder before update: {:#?}", p);
                    let _ = std::fs::remove_dir_all(p);
                }
            }
        }
    }

    for file in &response.required_files {

        let path: PathBuf = match &file.install_location {
            update_protocol::InstallLocation::AbsolutePath(path) => path.into(),
            _ => return false
        };

        if path.exists() && Path::new("sd:/installing.tmpfile").exists() && path.extension().unwrap_or_default() != "nro" {
            continue;
        }
        match TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, PORT + 1), std::time::Duration::new(10, 0)) { 
            Ok(mut stream) => {
                let mut buf = vec![];
                let _ = stream.write_all(&u64::to_be_bytes(file.download_index));
                if let Err(e) = stream.read_to_end(&mut buf) {
                    println!("[updater] Error downloading file: {}", e);
                    return false
                }

                println!("Downloaded {:#?}", path.clone());
    
                if installer.install_file(path, buf).is_err() {
                    return false
                }
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            Err(e) => {
                println!("[updater] Failed to connect to port {}", PORT + 1);
                println!("Err: {}", e);
                /* Hacky solution to descriptor table filling up */
                if e.to_string().contains("os error 24") {
                    println!("Recovering download...");
                    std::fs::File::create(Path::new("sd:/installing.tmpfile")).unwrap();
                    skyline::nn::oe::RestartProgramNoArgs();
                }
                return false
            }
        };
    }
    println!("[updater] finished updating plugin.");
    let _ = std::fs::remove_file("sd:/installing.tmpfile");
    true
}

/// Install an update with a custom installer implementation
pub fn custom_check_update<I>(ip: IpAddr, name: &str, version: &str, allow_beta: bool, installer: &I) -> bool
    where I: Installer,
{
    match TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, PORT), std::time::Duration::new(10, 0)) {
        Ok(mut stream) =>  {
            if let Ok(packet) = serde_json::to_string(&Request::Update {
                beta: Some(allow_beta),
                plugin_name: name.to_owned(),
                plugin_version: version.to_owned(),
                options: None,
            }) {
                let _ = stream.write_fmt(format_args!("{}\n", packet));
                let mut string = String::new();
                let _ = stream.read_to_string(&mut string);

                if let Ok(response) = serde_json::from_str::<UpdateResponse>(&string) {
                    match response.code {
                        ResponseCode::NoUpdate => return false,
                        ResponseCode::Update => {
                            if installer.should_update(&response) {
                                let success = update(ip, &response, installer);

                                if !success {
                                    println!("[{} updater] Failed to install update, files may be left in a broken state.", name);
                                }

                                success
                            } else {
                                false
                            }
                        }
                        ResponseCode::InvalidRequest => {
                            println!("[{} updater] Failed to send a valid request to the server", name);
                            false
                        }
                        ResponseCode::PluginNotFound => {
                            println!("Plugin '{}' could not be found on the update server", name);
                            false
                        }
                        _ => {
                            println!("Unexpected response");
                            false
                        }
                    }
                } else {
                    println!("[{} updater] Failed to parse update server response: {:?}", name, string);
                    false
                }
            } else {
                println!("[{} updater] Failed to encode packet", name);
                false
            }
        }
        Err(e) => {
            println!("[{} updater] Failed to connect to update server {}", name, ip);
            println!("[{} updater] {:?}", name, e);
            false
        }
    }
}

/// Install an update using the default installer
///
/// ## Args
/// * ip - IP address of server
/// * name - name of plugin to update
/// * version - current version of plugin
/// * allow_beta - allow beta versions to be offered
pub fn check_update(ip: IpAddr, name: &str, version: &str, allow_beta: bool) -> bool {
    custom_check_update(ip, name, version, allow_beta, &DefaultInstaller)
}

pub fn get_update_info(ip: IpAddr, name: &str, version: &str, allow_beta: bool) -> Option<UpdateResponse> {
    match TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, PORT), std::time::Duration::new(10, 0)) {
        Ok(mut stream) =>  {
            if let Ok(packet) = serde_json::to_string(&Request::Update {
                beta: Some(allow_beta),
                plugin_name: name.to_owned(),
                plugin_version: version.to_owned(),
                options: None,
            }) {
                let _ = stream.write_fmt(format_args!("{}\n", packet));
                let mut string = String::new();
                let _ = stream.read_to_string(&mut string);

                if let Ok(response) = serde_json::from_str::<UpdateResponse>(&string) {
                    Some(response)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

pub fn install_update(ip: IpAddr, info: &UpdateResponse) -> bool {
    update(ip, info, &DefaultInstaller)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_install() {
        println!("{}", serde_json::to_string(&Request::Update { plugin_name: "test_name".into(), plugin_version: "1.0.0".into(), beta: None, options: None }).unwrap());
        check_update("127.0.0.1".parse().unwrap(), "test_plugin", "0.9.0", true);
    }
}
