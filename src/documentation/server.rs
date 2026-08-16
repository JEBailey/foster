use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct ServeOptions {
    pub port: u16,
    pub open_browser: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 8000,
            open_browser: true,
        }
    }
}

pub fn serve(directory: impl AsRef<Path>, options: ServeOptions) -> io::Result<()> {
    let root = fs::canonicalize(directory.as_ref())?;
    if !root.join("index.html").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("`{}` does not contain index.html", root.display()),
        ));
    }
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        options.port,
    ))?;
    let url = format!("http://{}", listener.local_addr()?);
    println!("serving documentation at {url} (press Ctrl-C to stop)");
    if options.open_browser {
        open_browser(&url)?;
    }
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let _ = respond(&root, &mut stream);
            }
            Err(error) => eprintln!("warning: documentation request failed: {error}"),
        }
    }
    Ok(())
}

fn respond(root: &Path, stream: &mut TcpStream) -> io::Result<()> {
    let mut request = [0_u8; 8192];
    let read = stream.read(&mut request)?;
    let request = String::from_utf8_lossy(&request[..read]);
    let Some(target) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        return response(
            stream,
            "400 Bad Request",
            "text/plain; charset=utf-8",
            b"bad request",
        );
    };
    let Some(path) = requested_file(root, target) else {
        return response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        );
    };
    match fs::read(&path) {
        Ok(body) => response(stream, "200 OK", content_type(&path), &body),
        Err(error) if error.kind() == io::ErrorKind::NotFound => response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
        Err(error) => Err(error),
    }
}

fn requested_file(root: &Path, target: &str) -> Option<PathBuf> {
    let target = target.split(['?', '#']).next()?.trim_start_matches('/');
    let relative = if target.is_empty() {
        Path::new("index.html")
    } else {
        Path::new(target)
    };
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let candidate = root.join(relative);
    if candidate.is_dir() {
        Some(candidate.join("index.html"))
    } else {
        Some(candidate)
    }
}

fn response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_paths_cannot_escape_the_documentation_root() {
        let root = Path::new("documentation");
        assert_eq!(requested_file(root, "/"), Some(root.join("index.html")));
        assert_eq!(
            requested_file(root, "/modules/core.html?x=1"),
            Some(root.join("modules/core.html"))
        );
        assert_eq!(requested_file(root, "/../secret"), None);
    }
}
