//! Admitted full compiled images. Raw Wasmtime code/pre handles never escape
//! these owners; every Store also keeps an owner outside its early-dropped data.

use anyhow::{Context as _, ensure};
use std::{path::Path, sync::Arc};
use wasmtime::{
    Engine, Store,
    component::{Component, Linker},
};

use crate::{
    budget::{self, Lease},
    host::{CallState, Library, LibraryPre},
};

pub(crate) struct Image {
    // Field order is part of the resource contract: unmap before release.
    component: Component,
    bytes: usize,
    _lease: Lease,
}

impl Image {
    /// # Safety
    /// Only131's private immutable artifact from the owned successful helper,
    /// with exact build/configuration, framing, nonce and checksum proof.
    pub(crate) unsafe fn deserialize_owned(
        engine: &Engine,
        path: &Path,
        expected: usize,
    ) -> anyhow::Result<Arc<Self>> {
        // SAFETY: forward the caller's already verified producer contract.
        unsafe { Self::deserialize_budgeted(engine, path, expected, &budget::CODE) }
    }

    /// Same trusted-producer contract; private budget injection isolates tests.
    unsafe fn deserialize_budgeted(
        engine: &Engine,
        path: &Path,
        expected: usize,
        budget: &'static budget::Budget,
    ) -> anyhow::Result<Arc<Self>> {
        let actual = usize::try_from(std::fs::metadata(path)?.len())?;
        ensure!(actual == expected, "owned compiler artifact size changed");
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        ensure!(page > 0, "cannot query host image page size");
        let charge = budget::rounded_image_bytes(actual, usize::try_from(page)?)?;
        // This local precedes Component, so all error paths unmap first.
        let lease = budget.lease();
        lease.grow(charge)?;
        // SAFETY: caller proves the same private artifact trust boundary.
        let component = unsafe { Component::deserialize_file(engine, path) }
            .context("deserialize owned compiler artifact")?;
        let range = component.image_range();
        let bytes = (range.end as usize)
            .checked_sub(range.start as usize)
            .context("invalid compiled image range")?;
        ensure!(
            bytes == actual,
            "compiled image does not match its reserved full file span"
        );
        Ok(Arc::new(Self {
            component,
            bytes,
            _lease: lease,
        }))
    }

    pub(crate) fn link(self: Arc<Self>, linker: &Linker<CallState>) -> anyhow::Result<Arc<Pre>> {
        let inner = LibraryPre::new(linker.instantiate_pre(&self.component)?)?;
        Ok(Arc::new(Pre { inner, image: self }))
    }
}

pub(crate) struct Pre {
    // Drop all pre/module references before the final admitted Image owner.
    inner: LibraryPre<CallState>,
    image: Arc<Image>,
}

// No raw Component/LibraryPre getter or Deref: those cloneable handles could
// outlive their lease. Escaping WasmBacktrace/FrameInfo can also own Modules;
// the shared engine deliberately disables backtraces and coredumps, and no
// supported host path uses force_capture. Changing that policy requires
// pairing such errors with this owner or converting them to owned text.

impl Pre {
    pub(crate) fn instantiate(&self, store: &mut Store<CallState>) -> anyhow::Result<Library> {
        self.inner.instantiate(store)
    }

    pub(crate) fn image_bytes(&self) -> usize {
        self.image.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{Budget, Retained};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn full_image_admission_and_pre_store_aliases_release_only_after_last_owner() {
        let engine = crate::compiler_engine::engine().unwrap();
        // Test-only trusted producer: this exact engine creates its own image.
        // Production never compiles in-process or accepts serialized input.
        let bytes = engine.precompile_component(b"\0asm\x0d\0\x01\0").unwrap();
        let mut artifact = tempfile::NamedTempFile::new().unwrap();
        artifact.write_all(&bytes).unwrap();
        artifact
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o400))
            .unwrap();
        let artifact = artifact.into_temp_path();
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let charge = budget::rounded_image_bytes(bytes.len(), page).unwrap();
        let budget = Budget::for_test(charge - 1);
        // SAFETY: the private immutable test artifact was generated above by
        // this very engine, never externally supplied serialized code.
        assert!(
            unsafe { Image::deserialize_budgeted(&engine, &artifact, bytes.len(), budget) }
                .is_err()
        );
        assert_eq!(budget.snapshot().1, 0);
        budget.limit_for_test(charge as i64);

        for store_last in [false, true] {
            let image =
                unsafe { Image::deserialize_budgeted(&engine, &artifact, bytes.len(), budget) }
                    .unwrap();
            assert_eq!(image.bytes, bytes.len());
            assert_eq!(budget.snapshot().1, charge as i64);
            let weak = Arc::downgrade(&image);
            let linker = Linker::<()>::new(&engine);
            let raw_pre = linker.instantiate_pre(&image.component).unwrap();
            let pre = Arc::new(Retained {
                resource: raw_pre,
                _owners: Arc::clone(&image),
            });
            let prepared = Arc::clone(&pre);
            let mut store = Retained {
                resource: Store::new(&engine, ()),
                _owners: Arc::clone(&image),
            };
            store.resource.set_epoch_deadline(1);
            pre.resource.instantiate(&mut store.resource).unwrap();
            let kept = Arc::new(store);
            let active = Arc::clone(&kept);
            drop(image);
            drop(pre);
            drop(kept);
            assert!(weak.upgrade().is_some());
            if store_last {
                drop(prepared);
                assert_eq!(budget.snapshot().1, charge as i64);
                drop(active);
            } else {
                drop(active);
                assert_eq!(budget.snapshot().1, charge as i64);
                drop(prepared);
            }
            assert!(weak.upgrade().is_none());
            assert_eq!(budget.snapshot().1, 0);
        }

        let image = unsafe { Image::deserialize_budgeted(&engine, &artifact, bytes.len(), budget) }
            .unwrap();
        // Empty component has no required library exports. Link failure must
        // release the already admitted image as well as any generated pre.
        assert!(image.link(&Linker::<CallState>::new(&engine)).is_err());
        assert_eq!(budget.snapshot().1, 0);
    }
}
