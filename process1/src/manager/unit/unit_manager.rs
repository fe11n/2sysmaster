use super::job::{JobAffect, JobConf, JobKind, JobManager};
use super::unit_datastore::UnitDb;
use super::unit_file::UnitFile;
use super::unit_load::UnitLoad;
use super::unit_parser_mgr::{UnitConfigParser, UnitParserMgr};
use super::unit_relation_atom::UnitRelationAtom;
use super::unit_runtime::UnitRT;
use super::{UnitObj, UnitX};
use crate::manager::data::{DataManager, JobMode, UnitState};
use crate::manager::table::{TableOp, TableSubscribe};
use crate::manager::MngErrno;
use event::Events;
use nix::unistd::Pid;
use std::error::Error;
use std::rc::Rc;

// #[macro_use]
// use crate::unit_name_to_type;
//unitManger composition of units with hash map

pub trait UnitMngUtil {
    fn attach(&mut self, um: Rc<UnitManager>);
}

pub trait UnitSubClass: UnitObj + UnitMngUtil {
    fn into_unitobj(self: Box<Self>) -> Box<dyn UnitObj>;
}

#[macro_export]
macro_rules! declure_unitobj_plugin {
    ($unit_type:ty, $constructor:path, $name:expr, $level:expr) => {
        #[no_mangle]
        pub fn __unit_obj_create() -> *mut dyn $crate::manager::UnitSubClass {
            logger::init_log_with_default($name, $level);
            let construcotr: fn() -> $unit_type = $constructor;

            let obj = construcotr();
            let boxed: Box<dyn $crate::manager::UnitSubClass> = Box::new(obj);
            Box::into_raw(boxed)
        }
    };
}

//#[derive(Debug)]
pub struct UnitManagerX {
    states: Rc<UnitStates>,
    data: Rc<UnitManager>,
}

impl UnitManagerX {
    pub fn new(dm: Rc<DataManager>, event: Rc<Events>) -> UnitManagerX {
        let _dm = Rc::clone(&dm);
        let _um = UnitManager::new(Rc::clone(&_dm), event);
        UnitManagerX {
            states: Rc::new(UnitStates::new(Rc::clone(&_dm), Rc::clone(&_um))),
            data: _um,
        }
    }

    pub fn start_unit(&self, name: &str) -> Result<(), MngErrno> {
        self.data.start_unit(name)
    }

    pub fn stop_unit(&self, name: &str) -> Result<(), MngErrno> {
        self.data.stop_unit(name)
    }

    pub fn child_dispatch_sigchld(&self) -> Result<(), Box<dyn Error>> {
        self.data.db.child_dispatch_sigchld()
    }

    pub fn dispatch_load_queue(&self) {
        self.data.rt.dispatch_load_queue()
    }
}

//#[derive(Debug)]
pub struct UnitManager {
    // associated objects
    dm: Rc<DataManager>,
    event: Rc<Events>,

    file: Rc<UnitFile>,
    load: Rc<UnitLoad>,
    db: Rc<UnitDb>, // ALL UNIT STORE IN UNITDB,AND OTHER USE REF
    rt: Rc<UnitRT>,
    jm: Rc<JobManager>,
    unit_conf_parser_mgr: Rc<UnitParserMgr<UnitConfigParser>>,
}

impl UnitManager {
    pub fn child_watch_pid(&self, pid: Pid, id: &str) {
        self.db.child_add_watch_pid(pid, id)
    }

    pub fn child_unwatch_pid(&self, pid: Pid) {
        self.db.child_unwatch_pid(pid)
    }

    pub fn start_unit(&self, name: &str) -> Result<(), MngErrno> {
        if let Some(unit) = self.load.load_unit(name) {
            log::debug!("load unit success, send to job manager");
            self.jm.exec(
                &JobConf::new(Rc::clone(&unit), JobKind::JobStart),
                JobMode::JobReplace,
                &mut JobAffect::new(false),
            )?;
            Ok(())
        } else {
            return Err(MngErrno::MngErrInternel);
        }
    }

    pub fn stop_unit(&self, name: &str) -> Result<(), MngErrno> {
        if let Some(unit) = self.load.load_unit(name) {
            self.jm.exec(
                &JobConf::new(Rc::clone(&unit), JobKind::JobStop),
                JobMode::JobReplace,
                &mut JobAffect::new(false),
            )?;
            Ok(())
        } else {
            return Err(MngErrno::MngErrInternel);
        }
    }

    pub fn load(&self, name: &str) -> Option<Rc<UnitX>> {
        self.load.load_unit(name)
    }

    pub(in crate::manager) fn new(dm: Rc<DataManager>, event: Rc<Events>) -> Rc<UnitManager> {
        let _dm = Rc::clone(&dm);
        let _event = Rc::clone(&event);
        let _file = Rc::new(UnitFile::new());
        let _db = Rc::new(UnitDb::new());
        let rt = Rc::new(UnitRT::new());
        let unit_conf_parser_mgr = Rc::new(UnitParserMgr::default());
        _file.init_lookup_path();

        let _load = Rc::new(UnitLoad::new(
            Rc::clone(&_dm),
            Rc::clone(&_file),
            Rc::clone(&_db),
            Rc::clone(&rt),
            Rc::clone(&unit_conf_parser_mgr),
        ));

        let um = Rc::new(UnitManager {
            dm,
            event,

            file: Rc::clone(&_file),
            load: Rc::clone(&_load),
            db: Rc::clone(&_db),
            rt: Rc::clone(&rt),
            jm: Rc::new(JobManager::new(Rc::clone(&_db), Rc::clone(&_event))),
            unit_conf_parser_mgr: Rc::clone(&unit_conf_parser_mgr),
        });

        _load.set_um(um.clone());
        um
    }
}

//#[derive(Debug)]
struct UnitStates {
    name: String,            // key for table-subscriber
    data: Rc<UnitStatesSub>, // data for table-subscriber
}

impl UnitStates {
    pub(self) fn new(dm: Rc<DataManager>, um: Rc<UnitManager>) -> UnitStates {
        let us = UnitStates {
            name: String::from("UnitStates"),
            data: Rc::new(UnitStatesSub::new(um)),
        };
        us.register(&dm);
        us
    }

    fn register(&self, dm: &DataManager) {
        let subscriber = Rc::clone(&self.data);
        let register_result = dm.register_unit_state(self.name.clone(), subscriber);
        if let Some(_r) = register_result {
            log::info!("TableSubcribe for {} is already register", &self.name);
        } else {
            log::info!("register  TableSubcribe for {}  sucessfull", &self.name);
        }
    }
}

//#[derive(Debug)]
struct UnitStatesSub {
    um: Rc<UnitManager>,
}

impl TableSubscribe<String, UnitState> for UnitStatesSub {
    fn filter(&self, _op: &TableOp<String, UnitState>) -> bool {
        // everything is allowed
        true
    }

    fn notify(&self, op: &TableOp<String, UnitState>) {
        match op {
            TableOp::TableInsert(name, config) => self.insert_states(name, config),
            TableOp::TableRemove(name, _) => self.remove_states(name),
        }
    }
}

// the declaration "pub(self)" is for identification only.
impl UnitStatesSub {
    pub(self) fn new(um: Rc<UnitManager>) -> UnitStatesSub {
        UnitStatesSub { um }
    }

    pub(self) fn insert_states(&self, source: &str, state: &UnitState) {
        log::debug!("insert unit states source {}, state: {:?}", source, state);
        let unitx = if let Some(u) = self.um.db.units_get(source) {
            u
        } else {
            return;
        };

        self.um
            .jm
            .clone()
            .try_finish(&unitx, state.get_os(), state.get_ns(), state.get_flags())
            .unwrap();

        for other in self
            .um
            .db
            .dep_gets_atom(&unitx, UnitRelationAtom::UnitAtomTriggeredBy)
        {
            other.trigger(&unitx);
        }
    }

    pub(self) fn remove_states(&self, _source: &str) {
        todo!();
    }
}

#[cfg(test)]
mod tests {
    // use services::service::ServiceUnit;

    use super::*;
    use event::Events;
    use utils::logger;

    #[test]
    fn test_unit_load() {
        logger::init_log_with_console("test_unit_load", 4);
        log::info!("test");
        let dm_manager = Rc::new(DataManager::new());
        let _event = Rc::new(Events::new().unwrap());
        let um = UnitManager::new(dm_manager.clone(), Rc::clone(&_event));

        let unit_name = String::from("config.service");
        let unit = um.load(&unit_name);

        match unit {
            Some(_unit_obj) => assert_eq!(_unit_obj.get_id(), "config.service"),
            None => println!("test unit load, not fount unit: {}", unit_name),
        };
    }

    #[test]
    fn test_unit_start() {
        logger::init_log_with_console("test_unit_load", 4);
        let dm_manager = Rc::new(DataManager::new());
        let _event = Rc::new(Events::new().unwrap());
        let um = UnitManager::new(dm_manager.clone(), Rc::clone(&_event));

        let unit_name = String::from("config.service");
        let unit = um.load(&unit_name);

        match unit {
            Some(u) => {
                u.start().unwrap();
                log::debug!("unit start end!");
                u.stop().unwrap();
                log::debug!("unit stop end!");
            }
            None => println!("load unit failed"),
        }
    }
}
