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

//! load rules
//!

use super::*;
use crate::builtin::BuiltinCommand;
use crate::error::{Error, Result};
use crate::utils::*;
use basic::parse_util::parse_mode;
use basic::user_group_util::{parse_gid, parse_uid};
use fnmatch_regex;
use lazy_static::lazy_static;
use nix::unistd::{Group, User};
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// directories for searching rule files
pub const DEFAULT_RULES_DIRS: [&str; 4] = [
    "/etc/udev/rules.d",
    "/run/udev/rules.d",
    "/usr/local/lib/udev/rules.d",
    "/usr/lib/udev/rules.d",
];

impl Rules {
    /// load all rules under specified directories
    pub(crate) fn load_rules(
        dirs: &[&str],
        resolve_name_time: ResolveNameTime,
    ) -> Arc<RwLock<Rules>> {
        let rules = Arc::new(RwLock::new(Self::new(dirs, resolve_name_time)));

        Self::parse_rules(rules.clone());

        rules
    }

    /// enumerate all .rules file under the directories and generate the rules object
    pub(crate) fn new(dirs: &[&str], resolve_name_time: ResolveNameTime) -> Rules {
        let mut dirs_tmp = vec![];

        for d in dirs {
            dirs_tmp.push(d.to_string());
        }

        Rules {
            files: None,
            files_tail: None,
            dirs: dirs_tmp,
            resolve_name_time,
            users: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    /// enumerate and parse all rule files under rule directories
    pub(crate) fn parse_rules(rules: Arc<RwLock<Rules>>) {
        let dirs = rules.as_ref().read().unwrap().dirs.clone();
        for dir in dirs {
            let dir_path = std::path::Path::new(&dir);
            if !dir_path.exists() || !dir_path.is_dir() {
                log::warn!("Rule directory {} is invalid.", dir);
                continue;
            }

            let mut files: Vec<String> = vec![];

            for file in dir_path.read_dir().unwrap() {
                if file.is_err() {
                    log::warn!(
                        "Failed to read file under {}: {:?}.",
                        dir,
                        file.unwrap_err()
                    );
                    continue;
                }
                let buf = file.unwrap().path();
                let de = buf.as_os_str().to_str().unwrap();
                if !de.ends_with(".rules") {
                    log::warn!("Ignore file not ending with rules: {}", de);
                    continue;
                }
                files.push(de.to_string());
            }

            files.sort();

            for f in files {
                Self::parse_file(rules.clone(), f);
            }
        }
    }

    /// parse a single rule file, and insert it into rules
    pub(crate) fn parse_file(rules: Arc<RwLock<Rules>>, file_name: String) {
        log::debug!("Parsing rule file: {}", file_name);
        let file = RuleFile::load_file(file_name, Some(rules.clone()));
        Self::add_file(rules, file);
    }

    /// push the rule file into the tail of linked list
    pub(crate) fn add_file(rules: Arc<RwLock<Rules>>, file: Arc<RwLock<RuleFile>>) {
        let has_tail = rules.as_ref().read().unwrap().files_tail.is_none();
        if has_tail {
            rules.as_ref().write().unwrap().files = Some(file.clone());
        } else {
            rules
                .as_ref()
                .write()
                .unwrap()
                .files_tail
                .as_mut()
                .unwrap()
                .write()
                .unwrap()
                .next = Some(file.clone());
            file.write().unwrap().prev = rules.as_ref().read().unwrap().files_tail.clone();
        }

        rules.as_ref().write().unwrap().files_tail = Some(file);
    }

    /// if the user name has valid credential, insert it to rules
    pub(crate) fn resolve_user(&mut self, username: &str) -> Result<User> {
        if let Some(user) = self.users.get(username) {
            return Ok(user.clone());
        }

        match User::from_name(username) {
            Ok(user) => match user {
                Some(u) => Ok(u),
                None => Err(Error::RulesLoadError {
                    msg: format!("The user name {} has no credential.", username),
                }),
            },
            Err(e) => Err(Error::RulesLoadError {
                msg: format!("Failed to resolve user name {}: {}", username, e),
            }),
        }
    }

    /// if the group name has valid credential, insert it to rules
    pub(crate) fn resolve_group(&mut self, groupname: &str) -> Result<Group> {
        if let Some(group) = self.groups.get(groupname) {
            return Ok(group.clone());
        }

        match Group::from_name(groupname) {
            Ok(group) => match group {
                Some(g) => Ok(g),
                None => Err(Error::RulesLoadError {
                    msg: format!("The group name {} has no credential.", groupname),
                }),
            },
            Err(e) => Err(Error::RulesLoadError {
                msg: format!("Failed to resolve group name {}: {}", groupname, e),
            }),
        }
    }
}

impl RuleFile {
    /// rule file object is always stored in heap
    /// the pointer to rules is used for specific tokens, e.g., 'GOTO' and 'LABEL',
    /// which will directly modify some fields in rules
    pub(crate) fn load_file(
        file_name: String,
        rules: Option<Arc<RwLock<Rules>>>,
    ) -> Arc<RwLock<RuleFile>> {
        let rule_file = Arc::<RwLock<RuleFile>>::new(RwLock::<RuleFile>::new(Self::new(file_name)));

        // rule file is locked here, thus can not do read or write operations inside parse_lines
        rule_file
            .write()
            .unwrap()
            .parse_lines(rule_file.clone(), rules);

        rule_file
    }

    /// create a initial rule file object
    pub(crate) fn new(file_name: String) -> RuleFile {
        RuleFile {
            file_name,
            lines: None,
            lines_tail: None,
            prev: None,
            next: None,
        }
    }

    /// parse and load all available lines in the rule file
    /// the pointer to rules is used for specific tokens, e.g., 'GOTO' and 'LABEL',
    /// which will directly modify some fields in rules
    pub(crate) fn parse_lines(
        &mut self,
        self_ptr: Arc<RwLock<RuleFile>>,
        rules: Option<Arc<RwLock<Rules>>>,
    ) {
        let file = File::open(&self.file_name).unwrap();
        let reader = BufReader::new(file);

        let mut full_line = String::new();
        let mut offset = 0;
        for (line_number, line) in reader.lines().enumerate() {
            if let Err(e) = line {
                log::warn!("Read line failed in {} : {:?}", self.file_name, e);
                continue;
            }
            let line = line.unwrap();
            let line = line.trim_start().trim_end();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }

            if line.ends_with('\\') {
                full_line.push_str(line.strip_suffix('\\').unwrap());
                offset += 1;
            } else {
                full_line.push_str(line);
                let line = RuleLine::load_line(
                    full_line.to_string(),
                    (line_number + 1 - offset) as u32,
                    self_ptr.clone(),
                    rules.clone(),
                )
                .unwrap();
                self.add_line(line);
                full_line.clear();
                offset = 0;
            }
        }
    }

    /// push the rule line to the tail of linked list
    pub(crate) fn add_line(&mut self, line: Arc<RwLock<RuleLine>>) {
        if self.lines.is_none() {
            self.lines = Some(line.clone());
        } else {
            self.lines_tail.as_mut().unwrap().write().unwrap().next = Some(line.clone());
            line.write().unwrap().prev = self.lines_tail.clone();
        }

        self.lines_tail = Some(line);
    }
}

impl RuleLine {
    /// load a rule line
    pub(crate) fn new(line: String, line_number: u32, file: Arc<RwLock<RuleFile>>) -> RuleLine {
        RuleLine {
            line,
            line_number,

            r#type: RuleLineType::INITIAL,

            label: None,
            goto_label: None,
            goto_line: None,

            tokens: None,
            tokens_tail: None,

            file: Arc::downgrade(&file),

            next: None,
            prev: None,
        }
    }

    /// create a rule line object
    pub(crate) fn load_line(
        line: String,
        line_number: u32,
        file: Arc<RwLock<RuleFile>>,
        rules: Option<Arc<RwLock<Rules>>>,
    ) -> Result<Arc<RwLock<RuleLine>>> {
        lazy_static! {
            static ref RE_LINE: Regex =
                Regex::new("((?P<key>[^=,\"{+\\-!:\0\\s]+)(\\{(?P<attr>[^\\{\\}]+)\\})?\\s*(?P<op>[!:+-=]?=)\\s*\"(?P<value>[^\"]+)\"\\s*,?\\s*)+").unwrap();
            static ref RE_TOKEN: Regex =
                Regex::new("(?P<key>[^=,\"{+\\-!:\0\\s]+)(\\{(?P<attr>[^\\{\\}]+)\\})?\\s*(?P<op>[!:+-=]?=)\\s*\"(?P<value>[^\"]+)\"\\s*,?\\s*").unwrap();
        }

        let mut rule_line = RuleLine::new(line.clone(), line_number, file);

        if !RE_LINE.is_match(&line) {
            return Err(Error::RulesLoadError {
                msg: "Invalid rule line".to_string(),
            });
        }

        for token in RE_TOKEN.captures_iter(&line) {
            // through previous check through regular expression,
            // key, op, value must not be none
            // attr may be none in case of specific rule tokens
            let key = token.name("key").map(|k| k.as_str().to_string()).unwrap();
            let attr = token.name("attr").map(|a| a.as_str().to_string());
            let op = token.name("op").map(|o| o.as_str().to_string()).unwrap();
            let value = token.name("value").map(|v| v.as_str().to_string()).unwrap();
            log::debug!(
                "Capture a token:
line :  {}
key  :  {}
attr :  {}
op   :  {}
value:  {}",
                line,
                key,
                attr.clone().unwrap_or_default(),
                op,
                value,
            );

            // if the token is 'GOTO' or 'LABEL', parse_token will return a IgnoreError
            // the following tokens in this line, if any, will be skipped
            let rule_token = RuleToken::parse_token(key, attr, op, value, rules.clone())?;
            match rule_token.r#type {
                TokenType::Goto => {
                    rule_line.goto_label = Some(rule_token.value);
                    rule_line.r#type |= RuleLineType::HAS_GOTO;
                }
                TokenType::Label => {
                    rule_line.label = Some(rule_token.value);
                    rule_line.r#type |= RuleLineType::HAS_LABEL;
                }
                _ => {
                    rule_line.add_token(rule_token);
                }
            }
        }

        Ok(Arc::<RwLock<RuleLine>>::new(RwLock::<RuleLine>::new(
            rule_line,
        )))
    }

    /// push the rule token to the tail of linked list
    pub(crate) fn add_token(&mut self, rule_token: RuleToken) {
        let rule_token = Arc::<RwLock<RuleToken>>::new(RwLock::<RuleToken>::new(rule_token));
        if self.tokens.is_none() {
            self.tokens = Some(rule_token.clone());
        } else {
            self.tokens_tail.as_mut().unwrap().write().unwrap().next = Some(rule_token.clone());
            rule_token.write().unwrap().prev = self.tokens_tail.clone();
        }

        self.tokens_tail = Some(rule_token);
    }
}

impl RuleToken {
    /// create a rule token
    pub(crate) fn new(
        r#type: TokenType,
        op: OperatorType,
        attr: Option<String>,
        value: String,
    ) -> Result<RuleToken> {
        let mut match_type = MatchType::Invalid;
        let mut attr_subst_type = SubstituteType::Invalid;
        let mut value_regex = vec![];

        if r#type <= TokenType::MatchResult {
            if r#type == TokenType::MatchSubsystem
                && matches!(value.as_str(), "subsystem" | "bus" | "class")
            {
                match_type = MatchType::Subsystem;
            }

            if r#type < TokenType::MatchTest || r#type == TokenType::MatchResult {
                // compatible with udev rules
                for s in value.split('|') {
                    match fnmatch_regex::glob_to_regex(s) {
                        Ok(r) => {
                            value_regex.push(r);
                        }
                        Err(_) => {
                            return Err(Error::RulesLoadError {
                                msg: "Failed to parse token value to regex.".to_string(),
                            })
                        }
                    }
                }
            }
        }

        if matches!(r#type, TokenType::MatchAttr | TokenType::MatchParentsAttr) {
            attr_subst_type = match attr.clone().unwrap_or_default().parse::<SubstituteType>() {
                Ok(t) => t,
                Err(_) => {
                    return Err(Error::RulesLoadError {
                        msg: "Failed to parse the subsittution type of attribute.".to_string(),
                    });
                }
            }
        }

        Ok(RuleToken {
            r#type,
            op,
            match_type,
            value_regex,
            attr_subst_type,
            attr,
            value,
            prev: None,
            next: None,
        })
    }

    /// parse strings into a rule token
    pub fn parse_token(
        key: String,
        attr: Option<String>,
        op: String,
        value: String,
        rules: Option<Arc<RwLock<Rules>>>,
    ) -> Result<RuleToken> {
        let mut op = op.parse::<OperatorType>()?;
        let op_is_match = [OperatorType::Match, OperatorType::Nomatch].contains(&op);
        match key.as_str() {
            "ACTION" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ACTION' can not carry attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ACTION' can only take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchAction, op, None, value))?
            }
            "DEVPATH" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DEVPATH' can not carry attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DEVPATH' can only take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchDevpath, op, None, value))?
            }
            "KERNEL" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'KERNEL' can not carry attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'KERNEL' can only take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchKernel, op, attr, value))?
            }
            "SYMLINK" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SYMLINK' can not carry attribute.".to_string(),
                    });
                }
                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SYMLINK' can not take remove operator.".to_string(),
                    });
                }

                if !op_is_match {
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), false) {
                        log::warn!("{}", e);
                    }
                    Ok(RuleToken::new(TokenType::AssignDevlink, op, None, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchDevlink, op, None, value))?
                }
            }
            "NAME" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'NAME' can not carry attribute.".to_string(),
                    });
                }
                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'NAME' can not take remove operator.".to_string(),
                    });
                }

                if op == OperatorType::Add {
                    log::warn!("Key 'NAME' can only take '==', '!=', '=', or ':=' operator, change '+=' to '=' implicitly.");
                    op = OperatorType::Assign;
                }

                if !op_is_match {
                    if value.eq("%k") {
                        return Err(Error::RulesLoadError {
                            msg: "Ignore token NAME=\"%k\", as it takes no effect.".to_string(),
                        });
                    }
                    if value.is_empty() {
                        return Err(Error::RulesLoadError {
                            msg: "Ignore token NAME=\"\", as it takes no effect.".to_string(),
                        });
                    }
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), false) {
                        log::warn!("{}", e);
                    }

                    Ok(RuleToken::new(TokenType::AssignName, op, None, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchName, op, None, value))?
                }
            }
            "ENV" => {
                if attr.is_none() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ENV' must have attribute.".to_string(),
                    });
                }
                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ENV' can not take '-=' operator.".to_string(),
                    });
                }
                if op == OperatorType::AssignFinal {
                    log::warn!(
                        "Key 'ENV' can not take ':=' operator, change ':=' to '=' implicitly."
                    );
                    op = OperatorType::Assign;
                }

                if !op_is_match {
                    if matches!(
                        attr.as_ref().unwrap().as_str(),
                        "ACTION"
                            | "DEVLINKS"
                            | "DEVNAME"
                            | "DEVTYPE"
                            | "DRIVER"
                            | "IFINDEX"
                            | "MAJOR"
                            | "MINOR"
                            | "SEQNUM"
                            | "SUBSYSTEM"
                            | "TAGS"
                    ) {
                        return Err(Error::RulesLoadError {
                            msg: format!(
                                "Key 'ENV' has invalid attribute. '{}' can not be set.",
                                attr.as_ref().unwrap()
                            ),
                        });
                    }

                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), false) {
                        log::warn!("{}", e);
                    }

                    Ok(RuleToken::new(TokenType::AssignEnv, op, attr, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchEnv, op, attr, value))?
                }
            }
            "CONST" => {
                if attr.is_none() || matches!(attr.as_ref().unwrap().as_str(), "arch" | "virt") {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'CONST' has invalid attribute.".to_string(),
                    });
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'CONST' must take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchConst, op, attr, value))?
            }
            "TAG" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'TAG' can not have attribute.".to_string(),
                    });
                }

                if op == OperatorType::AssignFinal {
                    log::warn!(
                        "Key 'TAG' can not take ':=' operator, change ':=' to '=' implicitly."
                    );
                    op = OperatorType::Assign;
                }

                if !op_is_match {
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), true) {
                        log::warn!("{}", e);
                    }

                    Ok(RuleToken::new(TokenType::AssignTag, op, None, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchTag, op, None, value))?
                }
            }
            "SUBSYSTEM" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SUBSYSTEM' can not have attribute.".to_string(),
                    });
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SUBSYSTEM' must take match operator.".to_string(),
                    });
                }

                if matches!(value.as_str(), "bus" | "class") {
                    log::warn!("The value of key 'SUBSYSTEM' must be specified as 'subsystem'");
                }

                Ok(RuleToken::new(TokenType::MatchSubsystem, op, None, value))?
            }
            "DRIVER" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DRIVER' can not have attribute.".to_string(),
                    });
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DRIVER' must take match operator".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchDriver, op, None, value))?
            }
            "ATTR" => {
                if let Err(e) = check_attr_format(
                    key.as_str(),
                    attr.as_ref().unwrap_or(&"".to_string()).as_str(),
                ) {
                    log::warn!("{}", e);
                    return Err(e);
                }

                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ATTR' can not take remove operator.".to_string(),
                    });
                }

                if matches!(op, OperatorType::Add | OperatorType::AssignFinal) {
                    log::warn!(
                        "Key 'ATTR' can not take '+=' and ':=' operator, change to '=' implicitly."
                    );
                    op = OperatorType::Assign;
                }

                if !op_is_match {
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), false) {
                        log::warn!("{}", e);
                    }
                    Ok(RuleToken::new(TokenType::AssignAttr, op, attr, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchAttr, op, attr, value))?
                }
            }
            "SYSCTL" => {
                if let Err(e) = check_attr_format(
                    key.as_str(),
                    attr.as_ref().unwrap_or(&"".to_string()).as_str(),
                ) {
                    log::warn!("{}", e);
                    return Err(e);
                }

                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SYSCTL' can not take remove operator.".to_string(),
                    });
                }

                if matches!(op, OperatorType::Add | OperatorType::AssignFinal) {
                    log::warn!("Key 'SYSCTL' can not take '+=' and ':=' operator, change to '=' implicitly.");
                    op = OperatorType::Assign;
                }

                if !op_is_match {
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), false) {
                        log::warn!("{}", e);
                    }

                    Ok(RuleToken::new(TokenType::AssignAttr, op, attr, value))?
                } else {
                    Ok(RuleToken::new(TokenType::MatchAttr, op, attr, value))?
                }
            }
            "KERNELS" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'KERNELS' can not have attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'KERNELS' should take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(
                    TokenType::MatchParentsSubsystem,
                    op,
                    None,
                    value,
                ))?
            }
            "SUBSYSTEMS" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SUBSYSTEMS' can not have attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SUBSYSTEMS' should take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(
                    TokenType::MatchParentsSubsystem,
                    op,
                    None,
                    value,
                ))?
            }
            "DRIVERS" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DRIVERS' can not have attribute.".to_string(),
                    });
                }
                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'DRIVERS' should take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(
                    TokenType::MatchParentsDriver,
                    op,
                    None,
                    value,
                ))?
            }
            "ATTRS" => {
                if let Err(e) = check_attr_format(
                    key.as_str(),
                    attr.clone().unwrap_or("".to_string()).as_str(),
                ) {
                    log::warn!("{}", e);
                    return Err(e);
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'ATTRS' must take match operators.".to_string(),
                    });
                }

                if attr.clone().unwrap().starts_with("device/") {
                    log::warn!("'device' may be deprecated in future.");
                }

                if attr.clone().unwrap().starts_with("../") {
                    log::warn!("direct reference to parent directory may be deprecated in future.");
                }

                Ok(RuleToken::new(TokenType::MatchParentsAttr, op, attr, value))?
            }
            "TAGS" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'TAGS' can not have attribute.".to_string(),
                    });
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'TAGS' can only take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchParentsTag, op, None, value))?
            }
            "TEST" => {
                if attr.is_some() {
                    parse_mode(&attr.clone().unwrap()).map_err(|e| Error::RulesLoadError {
                        msg: format!("Key 'TEST' failed to parse mode: {}", e),
                    })?;
                }

                if let Err(e) = check_value_format(key.as_str(), value.as_str(), true) {
                    log::warn!("{}", e);
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'TEST' must tate match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchTest, op, attr, value))?
            }
            "PROGRAM" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'PROGRAM' can not have attribute.".to_string(),
                    });
                }

                if let Err(e) = check_value_format(key.as_str(), value.as_str(), true) {
                    log::warn!("{}", e);
                }

                if op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'PROGRAM' must have nonempty value.".to_string(),
                    });
                }

                if !op_is_match {
                    op = OperatorType::Match;
                }

                Ok(RuleToken::new(TokenType::MatchProgram, op, attr, value))?
            }
            "IMPORT" => {
                if attr.is_none() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'IMPORT' must have attribute.".to_string(),
                    });
                }

                if let Err(e) = check_value_format(key.as_str(), value.as_str(), true) {
                    log::warn!("{}", e);
                }

                if !op_is_match {
                    log::warn!("Key 'IMPORT' must take match operator, implicitly change to '='.");
                    op = OperatorType::Match;
                }

                if attr.as_ref().unwrap() == "file" {
                    Ok(RuleToken::new(TokenType::MatchImportFile, op, attr, value))?
                } else if attr.as_ref().unwrap() == "program" {
                    match value.parse::<BuiltinCommand>() {
                        Ok(_) => {
                            log::debug!("Parse the program into builtin command.");
                            Ok(RuleToken::new(
                                TokenType::MatchImportBuiltin,
                                op,
                                attr,
                                value,
                            ))?
                        }
                        Err(_) => Ok(RuleToken::new(
                            TokenType::MatchImportProgram,
                            op,
                            attr,
                            value,
                        ))?,
                    }
                } else if attr.as_ref().unwrap() == "builtin" {
                    if value.parse::<BuiltinCommand>().is_err() {
                        return Err(Error::RulesLoadError {
                            msg: format!("Invalid builtin command: {}", value),
                        });
                    }

                    Ok(RuleToken::new(
                        TokenType::MatchImportBuiltin,
                        op,
                        attr,
                        value,
                    ))?
                } else {
                    let token_type = match attr.as_ref().unwrap().as_str() {
                        "db" => TokenType::MatchImportDb,
                        "cmdline" => TokenType::MatchImportCmdline,
                        "parent" => TokenType::MatchImportParent,
                        _ => {
                            return Err(Error::RulesLoadError {
                                msg: "Key 'IMPORT' has invalid attribute.".to_string(),
                            })
                        }
                    };

                    Ok(RuleToken::new(token_type, op, attr, value))?
                }
            }
            "RESULT" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'RESULT' can not have attribute.".to_string(),
                    });
                }

                if !op_is_match {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'RESULT' must take match operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::MatchResult, op, attr, value))?
            }
            "OPTIONS" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'OPTIONS' can not have attribute.".to_string(),
                    });
                }
                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'OPTIONS' can not take match or remove operator.".to_string(),
                    });
                }
                if op == OperatorType::Add {
                    op = OperatorType::Assign;
                }

                match value.as_str() {
                    "string_escape=none" => Ok(RuleToken::new(
                        TokenType::AssignOptionsStringEscapeNone,
                        op,
                        None,
                        "".to_string(),
                    ))?,
                    "string_escape=replace" => Ok(RuleToken::new(
                        TokenType::AssignOptionsStringEscapeReplace,
                        op,
                        None,
                        "".to_string(),
                    ))?,
                    "db_persist" => Ok(RuleToken::new(
                        TokenType::AssignOptionsDbPersist,
                        op,
                        None,
                        "".to_string(),
                    ))?,
                    "watch" => Ok(RuleToken::new(
                        TokenType::AssignOptionsInotifyWatch,
                        op,
                        None,
                        "1".to_string(),
                    ))?,
                    "nowatch" => Ok(RuleToken::new(
                        TokenType::AssignOptionsInotifyWatch,
                        op,
                        None,
                        "0".to_string(),
                    ))?,
                    _ => {
                        if let Some(strip_value) = value.strip_prefix("static_node=") {
                            Ok(RuleToken::new(
                                TokenType::AssignOptionsStaticNode,
                                op,
                                None,
                                strip_value.to_string(),
                            ))?
                        } else if let Some(strip_value) = value.strip_prefix("link_priority=") {
                            if value["link_priority=".len()..].parse::<i32>().is_err() {
                                return Err(Error::RulesLoadError { msg: "Key 'OPTIONS' failed to parse link priority into a valid number.".to_string() });
                            }

                            Ok(RuleToken::new(
                                TokenType::AssignOptionsDevlinkPriority,
                                op,
                                None,
                                strip_value.to_string(),
                            ))?
                        } else if let Some(strip_value) = value.strip_prefix("log_level=") {
                            let level = if strip_value == "rest" {
                                "-1"
                            } else {
                                if let Err(e) = strip_value.parse::<i32>() {
                                    return Err(Error::RulesLoadError {
                                        msg: format!(
                                            "Key 'OPTIONS' failed to parse log level: {}",
                                            e
                                        ),
                                    });
                                }
                                strip_value
                            };

                            Ok(RuleToken::new(
                                TokenType::AssignOptionsLogLevel,
                                op,
                                None,
                                level.to_string(),
                            ))?
                        } else {
                            Err(Error::RulesLoadError {
                                msg: "Key 'OPTIONS' has invalid value.".to_string(),
                            })
                        }
                    }
                }
            }
            "OWNER" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'OWNER' can not have attribute.".to_string(),
                    });
                }

                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'OWNER' can not take match or remove operator.".to_string(),
                    });
                }

                if op == OperatorType::Add {
                    log::warn!("Key 'OWNER' can not take add operator, change to '=' implicitly.");
                    op = OperatorType::Assign;
                }

                if let Some(rules) = rules {
                    // only parse 'OWNER' token when rules object is provided.
                    if parse_uid(&value).is_ok() {
                        return Ok(RuleToken::new(TokenType::AssignOwnerId, op, attr, value))?;
                    }

                    let time = rules.as_ref().read().unwrap().resolve_name_time;
                    if time == ResolveNameTime::Early
                        && SubstituteType::Plain == value.parse::<SubstituteType>().unwrap()
                    {
                        let user = rules.as_ref().write().unwrap().resolve_user(&value)?;

                        return Ok(RuleToken::new(
                            TokenType::AssignOwnerId,
                            op,
                            attr,
                            user.uid.to_string(),
                        ))?;
                    } else if time != ResolveNameTime::Never {
                        // early or late
                        if let Err(e) = check_value_format("OWNER", value.as_str(), true) {
                            log::warn!("{}", e);
                        }

                        return Ok(RuleToken::new(TokenType::AssignOwner, op, attr, value))?;
                    }
                }

                Err(Error::IgnoreError {
                    msg: format!("Ignore resolving user name: 'OWNER=\"{}\"'", value),
                })
            }
            "GROUP" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'GROUP' can not have attribute.".to_string(),
                    });
                }

                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'GROUP' can not take match or remove operator.".to_string(),
                    });
                }

                if op == OperatorType::Add {
                    log::warn!("Key 'GROUP' can not take add operator, change to '=' implicitly.");
                    op = OperatorType::Assign;
                }

                if let Some(rules) = rules {
                    // only parse 'GROUP' token when rules object is provided.
                    if parse_gid(&value).is_ok() {
                        return Ok(RuleToken::new(TokenType::AssignGroupId, op, attr, value))?;
                    }

                    let time = rules.as_ref().read().unwrap().resolve_name_time;
                    if time == ResolveNameTime::Early
                        && SubstituteType::Plain == value.parse::<SubstituteType>().unwrap()
                    {
                        let group: Group = rules.as_ref().write().unwrap().resolve_group(&value)?;

                        return Ok(RuleToken::new(
                            TokenType::AssignGroupId,
                            op,
                            attr,
                            group.gid.to_string(),
                        ))?;
                    } else if time != ResolveNameTime::Never {
                        // early or late
                        if let Err(e) = check_value_format("GROUP", value.as_str(), true) {
                            log::warn!("{}", e);
                        }

                        return Ok(RuleToken::new(TokenType::AssignGroup, op, attr, value))?;
                    }
                }

                Err(Error::IgnoreError {
                    msg: format!("Ignore resolving user name: 'GROUP=\"{}\"'", value),
                })
            }
            "MODE" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'MODE' can not have attribute.".to_string(),
                    });
                }

                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'MODE' can not take match or remove operator.".to_string(),
                    });
                }

                if op == OperatorType::Add {
                    log::warn!("Key 'MODE' can not take add operator, change to '=' implicitly.");
                    op = OperatorType::Assign;
                }

                if parse_mode(&value).is_ok() {
                    Ok(RuleToken::new(TokenType::AssignModeId, op, None, value))?
                } else {
                    if let Err(e) = check_value_format(key.as_str(), value.as_str(), true) {
                        log::warn!("{}", e);
                    }

                    Ok(RuleToken::new(TokenType::AssignMode, op, None, value))?
                }
            }
            "SECLABEL" => {
                if attr.is_none() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SECLABEL' should take attribute.".to_string(),
                    });
                }

                if let Err(e) = check_value_format("SECLABEL", value.as_str(), true) {
                    log::warn!("{}", e);
                }

                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'SECLABEL' can not take match or remove operator.".to_string(),
                    });
                }

                if op == OperatorType::AssignFinal {
                    log::warn!(
                        "Key 'SECLABEL' can not take ':=' operator, change to '=' implicitly."
                    );
                    op = OperatorType::Assign;
                }

                Ok(RuleToken::new(TokenType::AssignSeclabel, op, attr, value))?
            }
            "RUN" => {
                if op_is_match || op == OperatorType::Remove {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'RUN' can not take match or remove operator.".to_string(),
                    });
                }

                if let Err(e) = check_value_format("RUN", value.as_str(), true) {
                    log::warn!("{}", e);
                }

                let attr_content = attr.clone().unwrap_or("".to_string());
                if attr.is_none() || attr_content == "program" {
                    Ok(RuleToken::new(TokenType::AssignRunProgram, op, None, value))?
                } else if attr_content == "builtin" {
                    if value.parse::<BuiltinCommand>().is_err() {
                        return Err(Error::RulesLoadError {
                            msg: format!("Key 'RUN' failed to parse builin command '{}'", value),
                        });
                    }

                    Ok(RuleToken::new(TokenType::AssignRunBuiltin, op, attr, value))?
                } else {
                    Err(Error::IgnoreError {
                        msg: format!(
                            "Ignore 'Run' token with invalid attribute {}.",
                            attr.unwrap()
                        ),
                    })
                }
            }
            "GOTO" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'GOTO' can not have attribute.".to_string(),
                    });
                }

                if op != OperatorType::Assign {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'GOTO' can not take assign operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::Goto, op, None, value))?
            }
            "LABEL" => {
                if attr.is_some() {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'LABEL' can not have attribute.".to_string(),
                    });
                }

                if op != OperatorType::Assign {
                    return Err(Error::RulesLoadError {
                        msg: "Key 'LABEL' can not take assign operator.".to_string(),
                    });
                }

                Ok(RuleToken::new(TokenType::Label, op, None, value))?
            }
            _ => Err(Error::RulesLoadError {
                msg: format!("Key '{}' is not supported.", key),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use basic::logger::init_log_to_console;
    use log::LevelFilter;

    use super::*;
    use std::{fs, path::Path, thread::JoinHandle};

    fn create_test_rules_dir(dir: &'static str) {
        assert!(fs::create_dir(dir).is_ok());
        assert!(fs::write(
            format!("{}/test.rules", dir),
            "ACTION == \"change\", SYMLINK += \"test1\"
ACTION == \"change\", SYMLINK += \"test11\", \\
SYMLINK += \"test111\"
ACTION == \"change\", SYMLINK += \"test1111\", \\
SYMLINK += \"test11111\", \\
SYMLINK += \"test111111\"",
        )
        .is_ok());
    }

    fn clear_test_rules_dir(dir: &'static str) {
        if Path::new(dir).exists() {
            assert!(fs::remove_dir_all(dir).is_ok());
        }
    }

    #[test]
    fn test_rules_new() {
        init_log_to_console("test_rules_new", LevelFilter::Debug);
        clear_test_rules_dir("test_rules_new");
        create_test_rules_dir("test_rules_new");
        let rules = Rules::load_rules(&DEFAULT_RULES_DIRS, ResolveNameTime::Early);
        println!("{}", rules.read().unwrap());
        clear_test_rules_dir("test_rules_new");
    }

    #[test]
    fn test_rules_file() {
        fs::write(
            "test_rules_file.rules",
            "ACTION == \"change\", SYMLINK+=\"test\"\nACTION != \"change\"\n",
        )
        .unwrap();
        RuleFile::load_file("test_rules_file.rules".to_string(), None);
        fs::remove_file("test_rules_file.rules").unwrap();
    }

    #[test]
    fn test_rules_token() {
        assert!(RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "add".to_string(),
            None,
        )
        .is_ok());

        assert!(RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "!=".to_string(),
            "add".to_string(),
            None
        )
        .is_ok());

        assert!(RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "*=".to_string(),
            "add".to_string(),
            None,
        )
        .is_err());

        assert!(RuleToken::parse_token(
            "ACTION".to_string(),
            Some("whatever".to_string()),
            "==".to_string(),
            "add".to_string(),
            None,
        )
        .is_err());
    }

    #[test]
    fn test_rules_token_regex() {
        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "add".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            ".?.*".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "?*".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "hello|?*|hello*|3279/tty[0-9]*".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ACTION".to_string(),
            None,
            "==".to_string(),
            "|hello|?*|hello*|3279/tty[0-9]*".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ATTR".to_string(),
            Some("whatever".to_string()),
            "==".to_string(),
            "hello".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ATTR".to_string(),
            Some("whatever$".to_string()),
            "==".to_string(),
            "hello".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);

        let t = RuleToken::parse_token(
            "ATTR".to_string(),
            Some("whatever%".to_string()),
            "==".to_string(),
            "hello".to_string(),
            None,
        )
        .unwrap();

        println!("{:?}", t);
    }

    #[test]
    fn test_rules_share_among_threads() {
        create_test_rules_dir("test_rules_share_among_threads");
        let rules = Rules::new(
            &["test_rules_new_1", "test_rules_new_2"],
            ResolveNameTime::Early,
        );
        let mut handles = Vec::<JoinHandle<()>>::new();
        (0..5).for_each(|i| {
            let rules_clone = rules.clone();
            let handle = std::thread::spawn(move || {
                println!("thread {}", i);
                println!("{}", rules_clone);
            });

            handles.push(handle);
        });

        for thread in handles {
            thread.join().unwrap();
        }

        clear_test_rules_dir("test_rules_share_among_threads");
    }

    #[test]
    #[ignore]
    fn test_resolve_user() {
        let mut rules = Rules::new(&[], ResolveNameTime::Early);
        assert!(rules.resolve_user("tss").is_ok());
        assert!(rules.resolve_user("root").is_ok());
        assert!(rules.users.contains_key("tss"));
        assert!(rules.users.contains_key("root"));
        assert!(rules.resolve_user("cjy").is_err());
    }
}
