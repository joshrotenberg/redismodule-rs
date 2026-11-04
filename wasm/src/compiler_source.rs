//! Long-running compilation borrows source through an owned spool, without a
//! second Redis-heap copy of the blocked command's retained argv. Persisted
//! sources already owned by the host share one Redis-allocated Vec with jobs.
use crate::compiler_protocol::MAX_INPUT;
use anyhow::ensure;
use std::{
    fs::File,
    io::{Read, Write},
    sync::Arc,
};
use tempfile::NamedTempFile;

pub(crate) enum Source {
    Spool(NamedTempFile, usize),
    Saved(Arc<Vec<u8>>),
}

impl Source {
    pub(crate) fn spool(bytes: &[u8]) -> anyhow::Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_INPUT,
            "component size must be between 1 and {MAX_INPUT} bytes"
        );
        let mut file = tempfile::Builder::new()
            .prefix("redis-wasm-source-")
            .tempfile()?;
        file.write_all(bytes)?;
        Ok(Self::Spool(file, bytes.len()))
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Spool(_, len) => *len,
            Self::Saved(bytes) => bytes.len(),
        }
    }

    pub(crate) fn reader(&self) -> anyhow::Result<Box<dyn Read + '_>> {
        Ok(match self {
            Self::Spool(file, _) => Box::new(File::open(file.path())?),
            Self::Saved(bytes) => Box::new(bytes.as_slice()),
        })
    }

    /// Call only after guest staging has finished. No guest/yield/user callback
    /// may run between materializing this lasting heap and command return.
    pub(crate) fn materialize(&mut self) -> anyhow::Result<()> {
        if let Self::Spool(file, len) = self {
            let mut bytes = Vec::with_capacity(*len);
            File::open(file.path())?
                .take(*len as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() == *len, "component source spool changed");
            *self = Self::Saved(Arc::new(bytes));
        }
        Ok(())
    }

    pub(crate) fn into_saved(mut self) -> anyhow::Result<Arc<Vec<u8>>> {
        self.materialize()?;
        match self {
            Self::Saved(bytes) => Ok(bytes),
            Self::Spool(..) => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_jobs_share_one_source_and_spools_materialize_once() {
        let bytes = Arc::new(vec![4; 1024]);
        let mut saved = Source::Saved(Arc::clone(&bytes));
        saved.materialize().unwrap();
        let owner = saved.into_saved().unwrap();
        assert!(Arc::ptr_eq(&bytes, &owner));
        let source = Source::spool(&bytes).unwrap();
        assert_eq!(source.into_saved().unwrap().as_slice(), bytes.as_slice());
        assert!(Source::spool(&[]).is_err());
    }
}
