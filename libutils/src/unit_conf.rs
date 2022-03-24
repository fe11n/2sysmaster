pub struct Conf(String, ConfValue);

#[derive(Debug)]
pub enum ConfValue {
    String(String),
    Interger(i64),
    Float(f64),
    Boolean(bool),
    Array(Vec<ConfValue>),
    Error(String),
}

pub enum SectionType {
    PRIVATE,
    PUB,
}
impl Conf {
    pub fn new(key: String, value: ConfValue) -> Self {
        Self(key, value)
    }
    pub fn get_key(&self) -> &str {
        &self.0
    }
    pub fn get_values(&self) -> Vec<ConfValue> {
        let mut ve: Vec<ConfValue> = Vec::new();
        match &self.1 {
            ConfValue::String(vs) => {
                let values = vs.split_whitespace();
                for v in values {
                    ve.push(ConfValue::String(v.to_string()));
                }
            }
            ConfValue::Interger(vi) => {
                ve.push(ConfValue::Interger(*vi));
            }
            ConfValue::Float(_) => todo!(),
            ConfValue::Boolean(_) => todo!(),
            ConfValue::Array(arr) => {
                for item in arr.iter() {
                    match item {
                        ConfValue::String(item_s) => {
                            ve.push(ConfValue::String(item_s.to_string()));
                        }
                        ConfValue::Interger(item_i) => {
                            ve.push(ConfValue::Interger(*item_i));
                        }
                        ConfValue::Float(item_f) => ve.push(ConfValue::Float(*item_f)),
                        ConfValue::Boolean(item_b) => {
                            ve.push(ConfValue::Boolean(*item_b));
                        }
                        ConfValue::Array(_) => continue, // not support nested
                        ConfValue::Error(_) => continue,
                    }
                }
            }
            ConfValue::Error(err) => {
                ve.push(ConfValue::String(err.to_string()));
            }
        }
        ve
    }
}

pub struct Section<Conf>(String, SectionType, Vec<Conf>);

impl Section<Conf> {
    pub fn new(name: String, section_type: SectionType) -> Self {
        Section(name, section_type, Vec::new())
    }

    pub fn get_section_name(&self) -> &str {
        &self.0
    }
    pub fn get_confs(&self) -> &Vec<Conf> {
        &self.2
    }
    pub fn add_conf(&mut self, conf: Conf) {
        self.2.push(conf);
    }
}

pub struct Confs {
    ctype: String,
    sections: Vec<Section<Conf>>,
}

impl Confs {
    /** need type ownership**/
    pub fn new(s_type: String) -> Self {
        Self {
            ctype: s_type,
            sections: Vec::new(),
        }
    }

    pub fn get_ctypes(&self) -> &str {
        &self.ctype
    }

    pub fn add_section(&mut self, section: Section<Conf>) {
        self.sections.push(section);
    }

    pub fn get_section(&mut self, name: &str) -> Option<&mut Section<Conf>> {
        for section in self.sections.iter_mut() {
            if section.get_section_name() == name {
                return Some(section);
            }
        }
        None
    }

    pub fn get_section_by_name(&self, name: &str) -> Option<&Section<Conf>> {
        for section in self.sections.iter() {
            if section.get_section_name() == name {
                return Some(section);
            }
        }
        None
    }

    pub fn get_sections(&self) -> &Vec<Section<Conf>> {
        &self.sections
    }
}

pub trait ConfFactory {
    fn product_confs(&self) -> Confs;
}
