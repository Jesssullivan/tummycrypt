//! One bounded raw-object reader per restore; no filters or per-file subprocesses.

use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use super::{git, oid};
use crate::{BulkloadRefusal, Result};

pub(super) struct BatchObjects {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
}

impl BatchObjects {
    pub(super) fn new(repository: &Path) -> Result<Self> {
        let mut child = git(repository)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take().map(BufReader::new);
        let reader = Self {
            child: Some(child),
            input,
            output,
        };
        if reader.input.is_none() || reader.output.is_none() {
            return Err(BulkloadRefusal::Io(None));
        }
        Ok(reader)
    }

    pub(super) fn copy_into(
        &mut self,
        value: &str,
        target: &mut impl Write,
        limit: Option<u64>,
    ) -> Result<u64> {
        if !oid(value) {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        let input = self.input.as_mut().ok_or(BulkloadRefusal::Io(None))?;
        input.write_all(value.as_bytes())?;
        input.write_all(b"\n")?;
        input.flush()?;
        let output = self.output.as_mut().ok_or(BulkloadRefusal::Io(None))?;
        let length = read_header(output, value)?;
        if limit.is_some_and(|limit| length > limit) {
            return Err(BulkloadRefusal::BudgetExceeded);
        }
        read_body(output, target, length)?;
        Ok(length)
    }

    pub(super) fn finish(mut self) -> Result<()> {
        self.close()
    }

    fn close(&mut self) -> Result<()> {
        // Closing stdin ends Git's request loop. Drain boundedly before wait so
        // a partially consumed body cannot leave the child blocked on its pipe.
        drop(self.input.take());
        let drained = self
            .output
            .take()
            .map(|mut output| std::io::copy(&mut output, &mut std::io::sink()))
            .transpose();
        let status = self.child.take().map(|mut child| child.wait()).transpose();
        if drained?.is_some_and(|bytes| bytes != 0)
            || status?.is_some_and(|status| !status.success())
        {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        Ok(())
    }
}

impl Drop for BatchObjects {
    fn drop(&mut self) {
        // Failure keeps caller-owned output for diagnosis. Never signal a child
        // or any agent; close, drain and reap only the process we spawned.
        let _ = self.close();
    }
}

fn read_header(reader: &mut impl Read, expected: &str) -> Result<u64> {
    let mut header = Vec::with_capacity(128);
    loop {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if byte == [b'\n'] {
            break;
        }
        if header.len() == 127 {
            return Err(BulkloadRefusal::GitInventoryMalformed);
        }
        header.extend_from_slice(&byte);
    }
    let header =
        std::str::from_utf8(&header).map_err(|_| BulkloadRefusal::GitInventoryMalformed)?;
    let mut fields = header.split(' ');
    if fields.next() != Some(expected) || fields.next() != Some("blob") {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let size = fields
        .next()
        .ok_or(BulkloadRefusal::GitInventoryMalformed)?;
    if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_digit()) || fields.next().is_some()
    {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    size.parse()
        .map_err(|_| BulkloadRefusal::GitInventoryMalformed)
}

fn read_body(reader: &mut impl Read, writer: &mut impl Write, length: u64) -> Result<()> {
    if std::io::copy(&mut (&mut *reader).take(length), writer)? != length {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    let mut terminator = [0];
    reader.read_exact(&mut terminator)?;
    if terminator != [b'\n'] {
        return Err(BulkloadRefusal::GitInventoryMalformed);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn framing_is_exact_and_bounded() {
        let oid = "1234567890123456789012345678901234567890";
        assert_eq!(
            read_header(&mut format!("{oid} blob 4\n").as_bytes(), oid).unwrap(),
            4
        );
        for header in [
            format!("{oid} tree 4\n"),
            format!("{oid} blob 4 extra\n"),
            format!("{oid} blob +4\n"),
            format!("{oid} missing\n"),
            "x".repeat(129),
            format!("{} blob 4\n", "a".repeat(40)),
        ] {
            assert!(read_header(&mut header.as_bytes(), oid).is_err());
        }
        assert!(read_body(&mut b"short".as_slice(), &mut Vec::new(), 9).is_err());
        assert!(read_body(&mut b"data!".as_slice(), &mut Vec::new(), 4).is_err());
        let mut bytes = Vec::new();
        read_body(&mut b"a\0b\n\n".as_slice(), &mut bytes, 4).unwrap();
        assert_eq!(bytes, b"a\0b\n");
    }

    #[test]
    fn one_child_streams_large_binary_multiple_blobs_and_reaps_after_limit() {
        let root = std::env::temp_dir().join(format!("bulkload-batch-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        super::super::output(git(&root).args(["init", "--bare", "--template="])).unwrap();
        let binary: Vec<u8> = (0..200_000)
            .map(|i| u8::try_from(i % 256).unwrap())
            .collect();
        let hash = super::super::input(git(&root).args(["hash-object", "-w", "--stdin"]), &binary)
            .unwrap();
        let value = std::str::from_utf8(&hash).unwrap().trim();
        let mut reader = BatchObjects::new(&root).unwrap();
        for _ in 0..3 {
            let mut received = Vec::new();
            assert_eq!(
                reader.copy_into(value, &mut received, None).unwrap(),
                200_000
            );
            assert_eq!(received, binary);
        }
        reader.finish().unwrap();
        let mut refused = BatchObjects::new(&root).unwrap();
        assert!(refused.copy_into(value, &mut Vec::new(), Some(16)).is_err());
        drop(refused);
        std::fs::remove_dir_all(root).unwrap();
    }
}
