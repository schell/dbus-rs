//! Experimental rewrite of Connection [unstable / experimental]
#![allow(missing_docs)]
#![allow(dead_code)]

use crate::{Error, Message, to_c_str, c_str_to_slice, MessageType};
use std::{ptr, str, time::Duration};
use std::ffi::CStr;
use std::os::raw::{c_void, c_int};
use crate::message::MatchRule;
use std::os::unix::io::RawFd;

#[derive(Debug)]
struct ConnHandle(*mut ffi::DBusConnection);

unsafe impl Send for ConnHandle {}
unsafe impl Sync for ConnHandle {}

impl Drop for ConnHandle {
    fn drop(&mut self) {
        unsafe {
            ffi::dbus_connection_close(self.0);
            ffi::dbus_connection_unref(self.0);
        }
    }
}

/// Which bus to connect to
#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq, Hash)]
pub enum BusType {
    /// The Session bus - local to every logged in session
    Session = ffi::DBusBusType::Session as isize,
    /// The system wide bus
    System = ffi::DBusBusType::System as isize,
    /// The bus that started us, if any
    Starter = ffi::DBusBusType::Starter as isize,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// A file descriptor, and an indication whether it should be read from, written to, or both.
pub struct Watch {
    pub fd: RawFd,
    pub read: bool,
    pub write: bool,
}

impl Watch {
    unsafe fn from_raw(watch: *mut ffi::DBusWatch) -> Self {
        let mut w = Watch { fd: ffi::dbus_watch_get_unix_fd(watch), read: false, write: false};
        let enabled = ffi::dbus_watch_get_enabled(watch) != 0;
        if enabled {
            let flags = ffi::dbus_watch_get_flags(watch);
            use std::os::raw::c_uint;
            w.read = (flags & ffi::DBUS_WATCH_READABLE as c_uint) != 0;
            w.write = (flags & ffi::DBUS_WATCH_WRITABLE as c_uint) != 0;
        }
        w
    }
}

/// Low-level connection - handles read/write to the socket
///
/// You probably do not need to worry about this as you would typically
/// use the various blocking and non-blocking "Connection" structs instead.
///
/// This version avoids dbus_connection_dispatch, and thus avoids
/// callbacks from that function. Instead the same functionality
/// is implemented in the various blocking and non-blocking "Connection" components.
///
/// Blocking operations are clearly marked as such, although if you
/// try to access the connection from several threads at the same time,
/// blocking might occur due to an internal mutex inside the dbus library.
#[derive(Debug)]
pub struct Channel {
    handle: ConnHandle,
}

impl Channel {
    #[inline(always)]
    pub (crate) fn conn(&self) -> *mut ffi::DBusConnection {
        self.handle.0
    }

    fn conn_from_ptr(ptr: *mut ffi::DBusConnection) -> Result<Channel, Error> {
        let handle = ConnHandle(ptr);

        /* No, we don't want our app to suddenly quit if dbus goes down */
        unsafe { ffi::dbus_connection_set_exit_on_disconnect(ptr, 0) };

        let c = Channel { handle };

        Ok(c)
    }


    /// Creates a new D-Bus connection.
    ///
    /// Blocking: until the connection is up and running. 
    pub fn get_private(bus: BusType) -> Result<Channel, Error> {
        let mut e = Error::empty();
        let b = match bus {
            BusType::Session => ffi::DBusBusType::Session,
            BusType::System => ffi::DBusBusType::System,
            BusType::Starter => ffi::DBusBusType::Starter,
        };
        let conn = unsafe { ffi::dbus_bus_get_private(b, e.get_mut()) };
        if conn.is_null() {
            return Err(e)
        }
        Self::conn_from_ptr(conn)
    }

    /// Creates a new D-Bus connection to a remote address.
    ///
    /// Note: for all common cases (System / Session bus) you probably want "get_private" instead.
    ///
    /// Blocking: until the connection is established.
    pub fn open_private(address: &str) -> Result<Channel, Error> {
        let mut e = Error::empty();
        let conn = unsafe { ffi::dbus_connection_open_private(to_c_str(address).as_ptr(), e.get_mut()) };
        if conn.is_null() {
            return Err(e)
        }
        Self::conn_from_ptr(conn)
    }

    /// Registers a new D-Bus connection with the bus.
    ///
    /// Note: `get_private` does this automatically, useful with `open_private`
    ///
    /// Blocking: until a "Hello" response is received from the server.
    pub fn register(&mut self) -> Result<(), Error> {
        // This function needs to take &mut self, because it changes unique_name and unique_name takes a &self
        let mut e = Error::empty();
        if unsafe { ffi::dbus_bus_register(self.conn(), e.get_mut()) == 0 } {
            Err(e)
        } else {
            Ok(())
        }
    }

    /// Gets whether the connection is currently open.
    pub fn is_connected(&self) -> bool {
        unsafe { ffi::dbus_connection_get_is_connected(self.conn()) != 0 }
    }

    /// Get the connection's unique name.
    ///
    /// It's usually something like ":1.54"
    pub fn unique_name(&self) -> Option<&str> {
        let c = unsafe { ffi::dbus_bus_get_unique_name(self.conn()) };
        if c.is_null() { return None; }
        let s = unsafe { CStr::from_ptr(c) };
        str::from_utf8(s.to_bytes()).ok()
    }


    /// Puts a message into libdbus out queue. Use "flush" or "read_write" to make sure it is sent over the wire.
    ///
    /// Returns a serial number than can be used to match against a reply.
    pub fn send(&self, msg: Message) -> Result<u32, ()> {
        let mut serial = 0u32;
        let r = unsafe { ffi::dbus_connection_send(self.conn(), msg.ptr(), &mut serial) };
        if r == 0 { return Err(()); }
        Ok(serial)
    }

    /// Sends a message over the D-Bus and waits for a reply. This is used for method calls.
    /// 
    /// Blocking: until a reply is received or the timeout expires.
    ///
    /// Note: In case of an error reply, this is returned as an Err(), not as a Ok(Message) with the error type.
    ///
    /// Note: In case pop_message and send_with_reply_and_block is called in parallel from different threads,
    /// they might race to retreive the reply message from the internal queue.
    pub fn send_with_reply_and_block(&self, msg: Message, timeout: Duration) -> Result<Message, Error> {
        let mut e = Error::empty();
        let response = unsafe {
            ffi::dbus_connection_send_with_reply_and_block(self.conn(), msg.ptr(),
                timeout.as_millis() as c_int, e.get_mut())
        };
        if response.is_null() {
            return Err(e);
        }
        Ok(Message::from_ptr(response, false))
    }

    /// Flush the queue of outgoing messages.
    /// 
    /// Blocking: until the outgoing queue is empty.
    pub fn flush(&self) { unsafe { ffi::dbus_connection_flush(self.conn()) } }

    /// Read and write to the connection.
    ///
    /// Incoming messages are put in the internal queue, outgoing messages are written.
    ///
    /// Blocking: If there are no messages, for up to timeout, or forever if timeout is None.
    /// For non-blocking behaviour, set timeout to Some(0).
    pub fn read_write(&self, timeout: Option<Duration>) -> Result<(), ()> {
        let t = timeout.map_or(-1, |t| t.as_millis() as c_int);
        if unsafe { ffi::dbus_connection_read_write(self.conn(), t) == 0 } {
            Err(())
        } else {
            Ok(())
        }
    }

    /// Removes a message from the incoming queue, or returns None if the queue is empty.
    ///
    /// Use "read_write" first, so that messages are put into the incoming queue.
    /// For unhandled messages, please call MessageDispatcher::default_dispatch to return
    /// default replies for method calls.
    pub fn pop_message(&self) -> Option<Message> {
        let mptr = unsafe { ffi::dbus_connection_pop_message(self.conn()) };
        if mptr.is_null() {
            None
        } else {
            Some(Message::from_ptr(mptr, false))
        }
    }

    /// Removes a message from the incoming queue, or waits until timeout if the queue is empty.
    ///
    pub fn blocking_pop_message(&self, timeout: Duration) -> Result<Option<Message>, Error> {
        if let Some(msg) = self.pop_message() { return Ok(Some(msg)) }
        self.read_write(Some(timeout)).map_err(|_|
            Error::new_failed("Failed to read/write data, disconnected from D-Bus?")
        )?;
        Ok(self.pop_message())
    }

    /// Get an up-to-date list of file descriptors to watch.
    ///
    /// Might be changed into something that allows for callbacks when the watch list is changed.
    pub fn watch_fds(&mut self) -> Result<Vec<Watch>, ()> {
        extern "C" fn add_watch_cb(watch: *mut ffi::DBusWatch, data: *mut c_void) -> u32 {
            unsafe {
                let wlist: &mut Vec<Watch> = &mut *(data as *mut _);
                wlist.push(Watch::from_raw(watch));
            }
            1
        }
        let mut wlist: Vec<Watch> = vec!();
        if unsafe { ffi::dbus_connection_set_watch_functions(self.conn(),
            Some(add_watch_cb), None, None, &mut wlist as *mut _ as *mut _, None) } == 0 { return Err(()) }
        assert!(unsafe { ffi::dbus_connection_set_watch_functions(self.conn(),
            None, None, None, ptr::null_mut(), None) } != 0);

        if wlist.len() == 2 && wlist[0].fd == wlist[1].fd {
            // This is always true in practice, see https://lists.freedesktop.org/archives/dbus/2019-July/017786.html
            wlist = vec!(Watch {
                fd: wlist[0].fd,
                read: wlist[0].read || wlist[1].read,
                write: wlist[0].write || wlist[1].write
            });
        }

        Ok(wlist)
    }
}

/// Abstraction over different connections
pub trait Sender {
    /// Schedules a message for sending.
    ///
    /// Returns a serial number than can be used to match against a reply.
    fn send(&self, msg: Message) -> Result<u32, ()>;
}

pub trait MatchingReceiver {
    type F;
    fn start_receive(&self, m: MatchRule<'static>, f: Self::F) -> u32;
    fn stop_receive(&self, id: u32) -> Option<(MatchRule<'static>, Self::F)>;
}

impl Sender for Channel {
    fn send(&self, msg: Message) -> Result<u32, ()> { Channel::send(self, msg) }
}

/// Handles what we need to be a good D-Bus citizen.
///
/// Call this if you have not handled the message yourself:
/// * It handles calls to org.freedesktop.DBus.Peer.
/// * For other method calls, it sends an error reply back that the method was unknown.
pub fn default_reply(m: &Message) -> Option<Message> {
    peer(&m).or_else(|| unknown_method(&m))
}

/// Replies if this is a call to org.freedesktop.DBus.Peer, otherwise returns None.
fn peer(m: &Message) -> Option<Message> {
    if let Some(intf) = m.interface() {
        if &*intf != "org.freedesktop.DBus.Peer" { return None; }
        if let Some(method) = m.member() {
            if &*method == "Ping" { return Some(m.method_return()) }
            if &*method == "GetMachineId" {
                let mut r = m.method_return();
                unsafe {
                    let id = ffi::dbus_get_local_machine_id();
                    if !id.is_null() {
                        r = r.append1(c_str_to_slice(&(id as *const _)).unwrap());
                        ffi::dbus_free(id as *mut _);
                        return Some(r)
                    }
                }
                return Some(m.error(&"org.freedesktop.DBus.Error.Failed".into(), &to_c_str("Failed to retreive UUID")))
            }
        }
        Some(m.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), &to_c_str("Method does not exist")))
    } else { None }
}

/// For method calls, it replies that the method was unknown, otherwise returns None.
fn unknown_method(m: &Message) -> Option<Message> {
    if m.msg_type() != MessageType::MethodCall { return None; }
    // if m.get_no_reply() { return None; } // The reference implementation does not do this?
    Some(m.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), &to_c_str("Path, Interface, or Method does not exist")))
}




#[test]
fn test_channel_send_sync() {
    fn is_send<T: Send>(_: &T) {}
    fn is_sync<T: Sync>(_: &T) {}
    let c = Channel::get_private(BusType::Session).unwrap();
    is_send(&c);
    is_sync(&c);
}

#[test]
fn channel_simple_test() {
    let mut c = Channel::get_private(BusType::Session).unwrap();
    assert!(c.is_connected());
    let fds = c.watch_fds().unwrap();
    println!("{:?}", fds);
    assert!(fds.len() == 1);
    let m = Message::new_method_call("org.freedesktop.DBus", "/", "org.freedesktop.DBus", "ListNames").unwrap();
    let reply = c.send(m).unwrap();
    let my_name = c.unique_name().unwrap();
    loop {
        while let Some(mut msg) = c.pop_message() {
            println!("{:?}", msg);
            if msg.get_reply_serial() == Some(reply) {
                let r = msg.as_result().unwrap();
                let z: crate::arg::Array<&str, _>  = r.get1().unwrap();
                for n in z {
                    println!("{}", n);
                    if n == my_name { return; } // Hooray, we found ourselves!
                }
                assert!(false);
            } else if let Some(r) = default_reply(&msg) {
                c.send(r).unwrap();
            }
        }
        c.read_write(Some(std::time::Duration::from_millis(100))).unwrap();
    }
}

#[test]
fn test_bus_type_is_compatible_with_set() {
    use std::collections::HashSet;

    let mut set: HashSet<BusType> = HashSet::new();
    set.insert(BusType::Starter);
    set.insert(BusType::Starter);

    assert_eq!(set.len(), 1);
    assert!(!set.contains(&BusType::Session));
    assert!(!set.contains(&BusType::System));
    assert!(set.contains(&BusType::Starter));
}
