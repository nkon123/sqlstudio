//! 잠금 — 다른 스레드가 잠근 채 패닉해도(잠금 오염) 계속 쓴다.
//!
//! `std` 의 `Mutex`·`RwLock` 은 잠근 스레드가 패닉하면 오염되어, 그 뒤 `lock().unwrap()` 이 모두 패닉한다.
//! 한 작업의 패닉이 앱 전체(같은 잠금을 쓰는 모든 명령)를 멈추게 하는 셈이다. 지키는 값은 대개 목록·지도라
//! 패닉 직전 상태로도 쓸 수 있으니 오염을 무시하고 안의 값을 쓴다. 패닉 자체는 로그에 남는다(앱의 패닉 훅).
//!
//! `x.lock().unwrap()` 대신 `x.lk()`, `read().unwrap()` 대신 `rd()`, `write().unwrap()` 대신 `wr()`.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub trait MutexExt<T: ?Sized> {
    fn lk(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexExt<T> for Mutex<T> {
    fn lk(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub trait RwLockExt<T: ?Sized> {
    fn rd(&self) -> RwLockReadGuard<'_, T>;
    fn wr(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockExt<T> for RwLock<T> {
    fn rd(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }
    fn wr(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn survives_poison() {
        let m = Arc::new(Mutex::new(vec![1]));
        let r = Arc::new(RwLock::new(1));
        let (m2, r2) = (m.clone(), r.clone());
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            let _w = r2.write().unwrap();
            panic!("일부러");
        })
        .join();
        assert!(m.is_poisoned() && r.is_poisoned());
        m.lk().push(2);
        *r.wr() += 1;
        assert_eq!(*m.lk(), vec![1, 2]);
        assert_eq!(*r.rd(), 2);
    }
}
