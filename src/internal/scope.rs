use std::mem;
use std::os::raw::c_void;
use std::marker::PhantomData;
use std::cell::{RefCell, UnsafeCell};
use nanny_sys::raw;
use nanny_sys::{Nan_Nested, Nan_Chained, Nan_EscapableHandleScope_Escape};
use internal::mem::{Handle, HandleInternal};
use internal::value::Tagged;
use internal::vm::Isolate;

pub trait ScopeInternal<'fun, 'block>: Sized {
    fn isolate(&self) -> &'fun Isolate;
    fn active_cell(&self) -> &RefCell<bool>;
}

pub trait Scope<'fun, 'block>: ScopeInternal<'fun, 'block> {
    fn nested<'outer, T, F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T>(&'outer self, f: F) -> T;
    fn chained<'outer, T, F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T>(&'outer self, f: F) -> T;
}

fn ensure_active<'fun, 'block, T: ScopeInternal<'fun, 'block>>(scope: &T) {
    if !*scope.active_cell().borrow() {
        panic!("illegal attempt to nest in inactive scope");
    }
}

pub struct RootScope<'fun, 'block> {
    isolate: &'fun Isolate,
    active: RefCell<bool>,
    phantom: PhantomData<&'block ()>
}

pub struct NestedScope<'fun, 'block> {
    isolate: &'fun Isolate,
    active: RefCell<bool>,
    phantom: PhantomData<&'block ()>
}

pub struct ChainedScope<'fun, 'block, 'parent> {
    isolate: &'fun Isolate,
    active: RefCell<bool>,
    v8: *mut raw::EscapableHandleScope,
    parent: PhantomData<&'parent ()>,
    phantom: PhantomData<&'block ()>
}

impl<'fun, 'block, 'parent> ChainedScope<'fun, 'block, 'parent> {
    pub fn escape<'me, T: Copy + Tagged>(&'me self, local: Handle<'block, T>) -> Handle<'parent, T> {
        let result: UnsafeCell<Handle<'parent, T>> = UnsafeCell::new(Handle::new(unsafe { mem::zeroed() }));
        unsafe {
            Nan_EscapableHandleScope_Escape((*result.get()).to_raw_mut_ref(), self.v8, local.to_raw());
            result.into_inner()
        }
    }
}

pub trait RootScopeInternal<'fun, 'block> {
    fn new(isolate: &'fun Isolate) -> RootScope<'fun, 'block>;
}

impl<'fun, 'block> RootScopeInternal<'fun, 'block> for RootScope<'fun, 'block> {
    fn new(isolate: &'fun Isolate) -> RootScope<'fun, 'block> {
        RootScope {
            isolate: isolate,
            active: RefCell::new(true),
            phantom: PhantomData
        }
    }
}

impl<'fun, 'block> Scope<'fun, 'block> for RootScope<'fun, 'block> {
    fn nested<'me, T, F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T>(&'me self, f: F) -> T {
        nest(self, f)
    }

    fn chained<'me, T, F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T>(&'me self, f: F) -> T {
        chain(self, f)
    }
}

extern "C" fn chained_callback<'fun, 'block, T, P, F>(out: &mut Box<Option<T>>,
                                                      parent: &P,
                                                      v8: *mut raw::EscapableHandleScope,
                                                      f: Box<F>)
    where P: Scope<'fun, 'block>,
          F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T
{
    let mut chained = ChainedScope {
        isolate: parent.isolate(),
        active: RefCell::new(true),
        v8: v8,
        parent: PhantomData,
        phantom: PhantomData
    };
    let result = f(&mut chained);
    **out = Some(result);
}

impl<'fun, 'block> ScopeInternal<'fun, 'block> for RootScope<'fun, 'block> {
    fn isolate(&self) -> &'fun Isolate { self.isolate }

    fn active_cell(&self) -> &RefCell<bool> {
        &self.active
    }
}

fn chain<'fun, 'block, 'me, T, S, F>(outer: &'me S, f: F) -> T
    where S: Scope<'fun, 'block>,
          F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T
{
    ensure_active(outer);
    let closure: Box<F> = Box::new(f);
    let callback: extern "C" fn(&mut Box<Option<T>>, &S, *mut raw::EscapableHandleScope, Box<F>) = chained_callback::<'fun, 'block, T, S, F>;
    let mut result: Box<Option<T>> = Box::new(None);
    {
        let out: &mut Box<Option<T>> = &mut result;
        { *outer.active_cell().borrow_mut() = false; }
        unsafe {
            let out: *mut c_void = mem::transmute(out);
            let closure: *mut c_void = mem::transmute(closure);
            let callback: extern "C" fn(&mut c_void, *mut c_void, *mut c_void, *mut c_void) = mem::transmute(callback);
            let this: *mut c_void = mem::transmute(outer);
            Nan_Chained(out, closure, callback, this);
        }
        { *outer.active_cell().borrow_mut() = true; }
    }
    result.unwrap()
}

fn nest<'fun, 'block, 'me, T, S, F>(outer: &'me S, f: F) -> T
    where S: ScopeInternal<'fun, 'block>,
          F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T
{
    ensure_active(outer);
    let closure: Box<F> = Box::new(f);
    let callback: extern "C" fn(&mut Box<Option<T>>, &'fun Isolate, Box<F>) = nested_callback::<'fun, T, F>;
    let mut result: Box<Option<T>> = Box::new(None);
    {
        let out: &mut Box<Option<T>> = &mut result;
        { *outer.active_cell().borrow_mut() = false; }
        unsafe {
            let out: *mut c_void = mem::transmute(out);
            let closure: *mut c_void = mem::transmute(closure);
            let callback: extern "C" fn(&mut c_void, *mut c_void, *mut c_void) = mem::transmute(callback);
            let isolate: *mut c_void = mem::transmute(outer.isolate());
            Nan_Nested(out, closure, callback, isolate);
        }
        { *outer.active_cell().borrow_mut() = true; }
    }
    result.unwrap()
}

extern "C" fn nested_callback<'fun, T, F>(out: &mut Box<Option<T>>,
                                          isolate: &'fun Isolate,
                                          f: Box<F>)
    where F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T
{
    let mut nested = NestedScope {
        isolate: isolate,
        active: RefCell::new(true),
        phantom: PhantomData
    };
    let result = f(&mut nested);
    **out = Some(result);
}

impl<'fun, 'block> Scope<'fun, 'block> for NestedScope<'fun, 'block> {
    fn nested<'me, T, F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T>(&'me self, f: F) -> T {
        nest(self, f)
    }

    fn chained<'outer, T, F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T>(&'outer self, f: F) -> T {
        chain(self, f)
    }
}

impl<'fun, 'block> ScopeInternal<'fun, 'block> for NestedScope<'fun, 'block> {
    fn isolate(&self) -> &'fun Isolate { self.isolate }

    fn active_cell(&self) -> &RefCell<bool> {
        &self.active
    }
}

impl<'fun, 'block, 'parent> Scope<'fun, 'block> for ChainedScope<'fun, 'block, 'parent> {
    fn nested<'me, T, F: for<'nested> FnOnce(&mut NestedScope<'fun, 'nested>) -> T>(&'me self, f: F) -> T {
        nest(self, f)
    }

    fn chained<'outer, T, F: for<'chained> FnOnce(&mut ChainedScope<'fun, 'chained, 'block>) -> T>(&'outer self, f: F) -> T {
        chain(self, f)
    }
}

impl<'fun, 'block, 'parent> ScopeInternal<'fun, 'block> for ChainedScope<'fun, 'block, 'parent> {
    fn isolate(&self) -> &'fun Isolate { self.isolate }

    fn active_cell(&self) -> &RefCell<bool> {
        &self.active
    }
}
