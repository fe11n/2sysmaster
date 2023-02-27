// Copyright (c) 2022 Huawei Technologies Co.,Ltd. All rights reserved.
//
// sysMaster is licensed under Mulan PSL v2.
// You can use this software according to the terms and conditions of the Mulan
// PSL v2.
// You may obtain a copy of Mulan PSL v2 at:
//         http://license.coscl.org.cn/MulanPSL2
// THIS SOFTWARE IS PROVIDED ON AN "AS IS" BASIS, WITHOUT WARRANTIES OF ANY
// KIND, EITHER EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
// NON-INFRINGEMENT, MERCHANTABILITY OR FIT FOR A PARTICULAR PURPOSE.
// See the Mulan PSL v2 for more details.

use event::{EventType, Events, Source};
use libcmdproto::proto::execute::ExecuterAction;
use libcmdproto::proto::ProstServerStream;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::io::RawFd;
use std::{os::unix::prelude::AsRawFd, rc::Rc};
use sysmaster::rel::{ReliLastFrame, Reliability};

pub(super) struct Commands<T> {
    // associated objects
    reli: Rc<Reliability>,
    command_action: Rc<T>,

    // owned objects
    fd: TcpListener,
}

impl<T> Commands<T> {
    pub(super) fn new(relir: &Rc<Reliability>, comm_action: T) -> Self {
        let addrs = [
            SocketAddr::from(([127, 0, 0, 1], 9526)),
            SocketAddr::from(([127, 0, 0, 1], 9527)),
        ];
        let fd = TcpListener::bind(&addrs[..]).unwrap();
        Commands {
            reli: Rc::clone(relir),
            command_action: Rc::new(comm_action),
            fd,
        }
    }
}

impl<T> Source for Commands<T>
where
    T: ExecuterAction,
{
    fn event_type(&self) -> EventType {
        EventType::Io
    }

    fn epoll_event(&self) -> u32 {
        (libc::EPOLLIN) as u32
    }

    fn dispatch(&self, _e: &Events) -> i32 {
        println!("Dispatching Command!");

        self.reli.set_last_frame1(ReliLastFrame::CmdOp as u32);
        match self.fd.incoming().next() {
            None => println!("None CommandRequest!"),
            Some(stream) => {
                println!("{stream:?}");
                let dispatch = ProstServerStream::new(stream.unwrap(), self.command_action.clone());
                dispatch.process().unwrap();
            }
        }
        self.reli.clear_last_frame();

        0
    }

    fn token(&self) -> u64 {
        let data: u64 = unsafe { std::mem::transmute(self) };
        data
    }

    fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
