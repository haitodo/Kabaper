use std::{marker::PhantomData, rc::Rc};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::Result;

/// Keep COM initialized on this thread until the owning interfaces are released.
pub(crate) struct ComApartment {
    owned: bool,
    _thread: PhantomData<Rc<()>>,
}

impl ComApartment {
    pub fn enter() -> Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // The GUI already owns an STA and can use the same WIC and shell APIs.
        if status != RPC_E_CHANGED_MODE {
            status.ok()?;
        }
        Ok(Self {
            owned: status.is_ok(),
            _thread: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}
