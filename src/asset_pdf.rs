//! Rasterize one PDF page in a filesystem/network-isolated process. Never fall
//! back to an unsandboxed parser if bubblewrap or poppler is unavailable.
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn page(pdf: Vec<u8>, page: u32) -> Result<Vec<u8>, String> {
    if !(1..=10000).contains(&page)
        || !pdf.starts_with(b"%PDF-")
        || pdf.len() as u64 > crate::assets::MAX_FILE
    {
        return Err("invalid_pdf_or_page".into());
    }
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .env_clear()
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib",
            "/lib64",
            "--tmpfs",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--dir",
            "/etc",
            "--ro-bind",
            "/etc/fonts",
            "/etc/fonts",
            "--chdir",
            "/tmp",
            "/usr/bin/pdftoppm",
            "-f",
            &page.to_string(),
            "-l",
            &page.to_string(),
            "-singlefile",
            "-scale-to",
            "1600",
            "-png",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            for (resource, value) in [
                (libc::RLIMIT_AS, 768 * 1024 * 1024),
                (libc::RLIMIT_CPU, 5),
                (libc::RLIMIT_FSIZE, 16 * 1024 * 1024),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "pdf_preview_requires_bubblewrap_and_poppler")?;
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&pdf);
    });
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let _ = writer.join();
    let bytes = reader
        .join()
        .map_err(|_| "pdf_render_failed")?
        .map_err(|_| "pdf_render_failed")?;
    if !success || bytes.len() > 8 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("pdf_page_unavailable_or_sandbox_failed".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_input_without_launching_a_renderer() {
        assert!(page(b"not a PDF".to_vec(), 1).is_err());
        assert!(page(b"%PDF-1.4".to_vec(), 0).is_err());
        assert!(page(b"%PDF-1.4".to_vec(), 10001).is_err());
    }

    #[test]
    #[ignore = "requires bubblewrap user namespaces and poppler; explicitly run on Linux"]
    fn sandbox_renders_a_single_page_fixture() {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >>",
            "<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut pdf = "%PDF-1.4\n".to_string();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str("xref\n0 5\n0000000000 65535 f \n");
        for offset in offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        ));
        assert!(page(pdf.clone().into_bytes(), 1)
            .unwrap()
            .starts_with(b"\x89PNG"));
        assert!(page(pdf.into_bytes(), 2).is_err());
    }
}
