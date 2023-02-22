use crate::socket_comm::SocketUnitComm;
use nix::unistd::Pid;
use std::rc::Rc;
use sysmaster::error::*;
use sysmaster::exec::{ExecCommand, ExecContext, ExecParameters};

pub(super) struct SocketSpawn {
    comm: Rc<SocketUnitComm>,
    exec_ctx: Rc<ExecContext>,
}

impl SocketSpawn {
    pub(super) fn new(comm: &Rc<SocketUnitComm>, exec_ctx: &Rc<ExecContext>) -> SocketSpawn {
        SocketSpawn {
            comm: comm.clone(),
            exec_ctx: exec_ctx.clone(),
        }
    }

    pub(super) fn start_socket(&self, cmdline: &ExecCommand) -> Result<Pid> {
        let params = ExecParameters::new();

        if let Some(unit) = self.comm.owner() {
            let um = self.comm.um();
            unit.prepare_exec()?;
            match um.exec_spawn(unit.id(), cmdline, &params, self.exec_ctx.clone()) {
                Ok(pid) => {
                    um.child_watch_pid(unit.id(), pid);
                    Ok(pid)
                }
                Err(_e) => {
                    log::error!("failed to start socket: {}", unit.id());
                    Err("spawn exec return error".to_string().into())
                }
            }
        } else {
            Err("spawn exec return error".to_string().into())
        }
    }
}
