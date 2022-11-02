#![allow(unused_imports,unused_assignments,unused_variables,unused_mut,dead_code)]

use std::cmp::max;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::LinkedList;
use std::net::SocketAddr;
use std::time::Duration;
use chrono::Local;
use serde::{Serialize,Deserialize};
use super::listener::{InnerNamingListener,NamingListenerCmd,ListenerItem};
use super::api_model::{QueryListResult};
use super::model::Instance;
use super::model::InstanceTimeInfo;
use super::model::InstanceUpdateTag;
use super::model::ServiceKey;
use super::model::UpdateInstanceType;
use super::service::Service;
use crate::utils::{gz_encode};

use actix::prelude::*;


pub struct NamingActor {
    service_map:HashMap<String,Service>,
    namespace_group_service:HashMap<String,BTreeSet<String>>,
    last_id:u64,
    listener_addr:Option<Addr<InnerNamingListener>>,
}

impl NamingActor {
    pub fn new(listener_addr:Option<Addr<InnerNamingListener>>) -> Self {
        Self {
            service_map: Default::default(),
            namespace_group_service: Default::default(),
            last_id:0u64,
            listener_addr:listener_addr,
        }
    }

    pub fn new_and_create(period:u64) -> Addr<Self> {
        Self::create(move |ctx|{
            let addr = ctx.address();
            let listener_addr = InnerNamingListener::new_and_create(period, Some(addr.clone()));
            Self::new(Some(listener_addr))
            //Self::new(None)
        })
    }

    pub(crate) fn get_service(&mut self,key:&ServiceKey) -> Option<&Service> {
        match self.service_map.get_mut(&key.get_join_service_name()){
            Some(v) => {
                Some(v)
            },
            None => None
        }
    }
    
    pub(crate) fn create_empty_service(&mut self,key:&ServiceKey) {
        let namspace_group = key.get_namespace_group();
        let ng_service_name = key.service_name.to_owned();
        if let Some(set) = self.namespace_group_service.get_mut(&namspace_group){
            set.insert(ng_service_name);
        }
        else{
            let mut set = BTreeSet::new();
            set.insert(ng_service_name);
            self.namespace_group_service.insert(namspace_group,set);
        }
        match self.get_service(key) {
            Some(_) => {},
            None => {
                let mut service = Service::default();
                let current_time = Local::now().timestamp_millis();
                service.service_name = key.service_name.to_owned();
                service.namespace_id = key.namespace_id.to_owned();
                service.group_name = key.group_name.to_owned();
                service.last_modified_millis = current_time;
                service.recalculate_checksum();
                self.service_map.insert(key.get_join_service_name(),service);
            }
        }
    }

    pub(crate) fn add_instance(&mut self,key:&ServiceKey,instance:Instance) -> UpdateInstanceType {
        let service = self.service_map.get_mut(&key.get_join_service_name()).unwrap();
        service.update_instance(instance,None)
    }

    pub fn remove_instance(&mut self,key:&ServiceKey ,cluster_name:&str,instance_id:&str) -> UpdateInstanceType {
        let service = self.service_map.get_mut(&key.get_join_service_name()).unwrap();
        service.remove_instance(cluster_name, instance_id)
    }

    pub fn update_instance(&mut self,key:&ServiceKey,mut instance:Instance,tag:Option<InstanceUpdateTag>) -> UpdateInstanceType {
        instance.init();
        assert!(instance.check_vaild());
        self.create_empty_service(key);
        let cluster_name = instance.cluster_name.clone();
        let service = self.service_map.get_mut(&key.get_join_service_name()).unwrap();
        service.update_instance(instance, tag)
    }

    pub fn get_instance(&self,key:&ServiceKey,cluster_name:&str,instance_id:&str) -> Option<Instance> {
        if let Some(service) = self.service_map.get(&key.get_join_service_name()) {
            return service.get_instance(cluster_name, instance_id);
        }
        None
    }

    pub fn get_instance_list(&self,key:&ServiceKey,cluster_names:Vec<String>,only_healthy:bool) -> Vec<Instance> {
        if let Some(service) = self.service_map.get(&key.get_join_service_name()) {
            return service.get_instance_list(cluster_names, only_healthy);
        }
        vec![]
    }

    pub fn get_instance_map(&self,key:&ServiceKey,cluster_names:Vec<String>,only_healthy:bool) -> HashMap<String,Vec<Instance>> {
        if let Some(service) = self.service_map.get(&key.get_join_service_name()) {
            return service.get_instance_map(cluster_names, only_healthy);
        }
        HashMap::new()
    }

    pub fn get_instance_list_string(&self,key:&ServiceKey,cluster_names:Vec<String>,only_healthy:bool) -> String {
        let clusters = (&cluster_names).join(",");
        let list = self.get_instance_list(key, cluster_names, only_healthy);
        QueryListResult::get_instance_list_string(clusters, key, list)
    }

    pub fn time_check(&mut self) -> (Vec<String>,Vec<String>) {
        let current_time = Local::now().timestamp_millis();
        let healthy_time = current_time - 15000;
        let offline_time = current_time - 30000;
        let mut remove_list = vec![];
        let mut update_list = vec![];
        for item in self.service_map.values_mut(){
            let (mut rlist,mut ulist)=item.time_check(healthy_time, offline_time);
            remove_list.append(&mut rlist);
            update_list.append(&mut ulist);
        }
        (remove_list,update_list)
    }

    pub fn get_service_list(&self,page_size:usize,page_index:usize,key:&ServiceKey) -> (usize,Vec<String>) {
        let offset = page_size * max(page_index-1,0);
        if let Some(set) = self.namespace_group_service.get(&key.get_namespace_group()){
            let size = set.len();
            return (size,set.into_iter().skip(offset).take(page_size).map(|e| e.to_owned()).collect::<Vec<_>>());
        }
        (0,vec![])
    }

    fn update_listener(&mut self,key:&ServiceKey,cluster_names:&Vec<String>,addr:SocketAddr,only_healthy:bool){
        if let Some(listener_addr) = self.listener_addr.as_ref() {
            let item = ListenerItem::new(cluster_names.clone(),only_healthy,addr);
            let msg = NamingListenerCmd::Add(key.clone(),item);
            listener_addr.do_send(msg);
        }
    }

    pub fn instance_time_out_heartbeat(&self,ctx:&mut actix::Context<Self>) {
        ctx.run_later(Duration::new(3,0), |act,ctx|{
            let addr = ctx.address();
            addr.do_send(NamingCmd::PeekListenerTimeout);
            act.instance_time_out_heartbeat(ctx);
        });
    }
}


#[derive(Debug,Message)]
#[rtype(result = "Result<NamingResult,std::io::Error>")]
pub enum NamingCmd {
    Update(Instance,Option<InstanceUpdateTag>),
    Delete(Instance),
    Query(Instance),
    QueryList(ServiceKey,Vec<String>,bool,Option<SocketAddr>),
    QueryListString(ServiceKey,Vec<String>,bool,Option<SocketAddr>),
    QueryServicePage(ServiceKey,usize,usize),
    PeekListenerTimeout,
    NotifyListener(ServiceKey,u64),
}

pub enum NamingResult {
    NULL,
    Instance(Instance),
    InstanceList(Vec<Instance>),
    InstanceListString(String),
    ServicePage((usize,Vec<String>)),
}

impl Actor for NamingActor {
    type Context = Context<Self>;

    fn started(&mut self,ctx: &mut Self::Context) {
        log::info!(" NamingActor started");
        let msg = NamingListenerCmd::InitNamingActor(ctx.address());
        if let Some(listener_addr) = self.listener_addr.as_ref() {
            listener_addr.do_send(msg);
        }
        self.instance_time_out_heartbeat(ctx);
    }

}

impl Handler<NamingCmd> for NamingActor {
    type Result = Result<NamingResult,std::io::Error>;

    fn handle(&mut self,msg:NamingCmd,ctx: &mut Context<Self>) -> Self::Result {
        match msg {
            NamingCmd::Update(instance,tag) => {
                self.update_instance(&instance.get_service_key(), instance, tag);
                Ok(NamingResult::NULL)
            },
            NamingCmd::Delete(instance) => {
                self.remove_instance(&instance.get_service_key(), &instance.cluster_name, &instance.id);
                Ok(NamingResult::NULL)
            },
            NamingCmd::Query(instance) => {
                if let Some(i) = self.get_instance(&instance.get_service_key(),&instance.cluster_name, &instance.id) {
                    return Ok(NamingResult::Instance(i));
                }
                Ok(NamingResult::NULL)
            },
            NamingCmd::QueryList(service_key,cluster_names,only_healthy,addr) => {
                if let Some(addr) = addr {
                    self.update_listener(&service_key, &cluster_names, addr,only_healthy);
                }
                let list = self.get_instance_list(&service_key, cluster_names, only_healthy);
                Ok(NamingResult::InstanceList(list))
            },
            NamingCmd::QueryListString(service_key,cluster_names,only_healthy,addr) => {
                println!("QUERY_LIST_STRING addr: {:?}",&addr);
                if let Some(addr) = addr {
                    self.update_listener(&service_key, &cluster_names, addr,only_healthy);
                }
                let data= self.get_instance_list_string(&service_key, cluster_names, only_healthy);
                Ok(NamingResult::InstanceListString(data))
            },
            NamingCmd::QueryServicePage(service_key, page_size, page_index) => {
                Ok(NamingResult::ServicePage(self.get_service_list(page_size, page_index, &service_key)))
            },
            NamingCmd::PeekListenerTimeout => {
                self.time_check();
                //self.notify_check();
                Ok(NamingResult::NULL)
            },
            NamingCmd::NotifyListener(service_key,id) => {
                if let Some(listener_addr) = self.listener_addr.as_ref() {
                    let map=self.get_instance_map(&service_key, vec![], false);
                    //notify listener
                    let msg = NamingListenerCmd::Notify(service_key,"".to_string(),map,id);
                    listener_addr.do_send(msg);
                }
                Ok(NamingResult::NULL)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::UdpSocket;
    use super::*;
    #[tokio::test()]
    async fn test01(){
        let listener_addr = InnerNamingListener::new_and_create(5000, None);
        let mut naming = NamingActor::new(None);
        let mut instance = Instance::new("127.0.0.1".to_owned(),8080);
        instance.namespace_id = "public".to_owned();
        instance.service_name = "foo".to_owned();
        instance.group_name = "DEFUALT".to_owned();
        instance.cluster_name= "DEFUALT".to_owned();
        instance.init();
        let key = instance.get_service_key();
        naming.update_instance(&key, instance, None);

        println!("-------------");
        let items = naming.get_instance_list(&key, vec!["DEFUALT".to_owned()], true);
        println!("DEFUALT list:{}",serde_json::to_string(&items).unwrap());
        let items = naming.get_instance_list(&key, vec![], true);
        println!("empty cluster list:{}",serde_json::to_string(&items).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(18000));
        naming.time_check();
        println!("-------------");
        let items = naming.get_instance_list(&key, vec![], false);
        println!("empty cluster list:{}",serde_json::to_string(&items).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(18000));
        naming.time_check();
        println!("-------------");
        let items = naming.get_instance_list(&key, vec![], false);
        println!("empty cluster list:{}",serde_json::to_string(&items).unwrap());

    }
}

