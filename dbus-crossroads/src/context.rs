
use crate::{MethodErr};
use crate::stdimpl::PropCtx;

pub struct Context {
    path: dbus::Path<'static>,
    interface: Option<dbus::strings::Interface<'static>>,
    method: dbus::strings::Member<'static>,
    message: dbus::Message,

    prop_ctx: Option<PropCtx>,
    reply: Option<dbus::Message>,
}

impl Context {
    pub fn new(msg: dbus::Message) -> Option<Self> {
        if msg.msg_type() != dbus::MessageType::MethodCall { return None; }
        let p = msg.path()?.into_static();
        let i = msg.interface().map(|i| i.into_static());
        let m = msg.member()?.into_static();
        Some(Context {
            path: p,
            interface: i,
            method: m,
            message: msg,
            reply: None,
            prop_ctx: None,
        })
    }

    pub fn check<R, F: FnOnce(&mut Context) -> Result<R, MethodErr>>(&mut self, f: F) -> Result<R, ()> {
        f(self).map_err(|e| {
            if !self.message.get_no_reply() {
                self.reply = Some(e.to_message(&self.message))
            };
        })
    }

    pub fn set_reply<F: FnOnce(&mut dbus::Message)>(&mut self, f: F) {
        if self.message.get_no_reply() { return; }
        if self.reply.is_some() { return; }
        let mut msg = self.message.method_return();
        f(&mut msg);
        self.reply = Some(msg);
    }

    pub fn flush_messages<S: dbus::channel::Sender>(&mut self, conn: &S) -> Result<(), ()> {
        if let Some(msg) = self.reply.take() {
            conn.send(msg)?;
        }
        Ok(())
    }

    pub fn path(&self) -> &dbus::Path<'static> { &self.path }
    pub fn interface(&self) -> Option<&dbus::strings::Interface<'static>> { self.interface.as_ref() }
    pub fn method(&self) -> &dbus::strings::Member<'static> { &self.method }
    pub fn message(&self) -> &dbus::Message { &self.message }

    pub fn has_reply(&self) -> bool { self.reply.is_some() }

    pub (crate) fn take_propctx(&mut self) -> Option<PropCtx> { self.prop_ctx.take() }
    pub (crate) fn give_propctx(&mut self, p: PropCtx) { self.prop_ctx = Some(p); }
}
