use ssh2::Session;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use tokio::time::{sleep, Duration};
use std::str::FromStr;
use std::error::Error;
use async_trait::async_trait;

use crate::market::AwsManager;


#[derive(Clone)]
pub struct Aws {
    client: aws_sdk_ec2::Client,
    key_name: String,
    // Path cannot be cloned, hence String
    key_location: String,
}

impl Aws {
    pub async fn new(aws_profile: &str, key_name: String) -> Aws {
        let credentials_provider = aws_config::profile::ProfileFileCredentialsProvider::builder()
            .profile_name(aws_profile)
            .build();
        let config = aws_config::from_env()
            .credentials_provider(credentials_provider)
            .load()
            .await;
        let client = aws_sdk_ec2::Client::new(&config);
        let key_location = "~/.ssh/".to_owned() + &key_name + ".pem";

        return Aws {
            client: client,
            key_name: key_name,
            key_location: key_location,
        };
    }

    /* AWS KEY PAIR UTILITY */

    pub async fn key_setup(&self) -> Result<(), Box<dyn Error>> {
        let key_check = self.check_key_pair().await;

        let file_check = Path::new(&self.key_location).exists();
        if !file_check {
            println!("key file not found");
        }

        if !(key_check || file_check) {
            self.create_key_pair().await?;
        } else if key_check && file_check {
            println!("Found existing keypair and pem file");
        } else {
            println!("ERROR: either key or file exists but not both");
            return Err("Key setup failed".into());
        }

        return Ok(());
    }

    async fn create_key_pair(&self) -> Result<(), Box<dyn Error>> {
        let resp = self.client
            .create_key_pair()
            .key_name(&self.key_name)
            .set_key_type(Some(aws_sdk_ec2::model::KeyType::Ed25519))
            .send()
            .await;

        let mut fingerprint = String::new();

        match resp {
            Ok(res) => {
                let key_material = res.key_material();
                if key_material.is_none() {
                    panic!("ERROR: extracting private key");
                }
                fingerprint.push_str(key_material.unwrap());
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
                return Err("Keypair creation failed".into());
            }
        }

        let mut file = File::create(Path::new(&self.key_location))?;
        file.write_all(fingerprint.as_bytes())?;
        let mut cmd = Command::new("chmod");
        cmd.arg("400");
        cmd.arg("/home/nisarg/one.pem");
        let _output = cmd.output();

        println!("Key-pair created, private key written to {}", self.key_location);

        Ok(())
    }

    async fn check_key_pair(&self) -> bool {
        let resp = self.client
            .describe_key_pairs()
            .key_names(&self.key_name)
            .send()
            .await;

        match resp {
            Ok(res) => match res.key_pairs() {
                None => {
                    println!("key not found");
                    return false;
                }
                Some(_) => {
                    return true;
                }
            },
            Err(_) => {
                println!("key not found");
                return false;
            }
        }
    }

    /* SSH UTILITY */

    async fn ssh_connect(&self, ip_address: String) -> Result<Session, Box<dyn Error + Send + Sync>> {
        let tcp = TcpStream::connect(&ip_address)?;

        let mut sess = Session::new()?;

        sess.set_tcp_stream(tcp);
        sess.handshake()?;
        sess.userauth_pubkey_file("ubuntu", None, Path::new(&self.key_location), None)?;
        println!("SSH connection established");
        return Ok(sess);
    }

    async fn run_enclave(&self, sess: &Session, url: &str, v_cpus: i32, mem: i64) -> Result<(), Box<dyn Error + Send + Sync>>{
        let mut channel = sess.channel_session()?;
        let mut s = String::new();
        channel
            .exec(
                &("echo -e '---\\nmemory_mib: ".to_owned() + &((mem-2048).to_string()) + "\\ncpu_count: " + &((v_cpus-2).to_string()) + "' >> /home/ubuntu/allocator_new.yaml"),
            )?;

        let _ = channel.read_to_string(&mut s);
        let _ = channel.wait_close();
        println!("{}", s);

        channel = sess.channel_session()?;
        channel
            .exec(
                &("sudo cp /home/ubuntu/allocator_new.yaml /etc/nitro_enclaves/allocator.yaml"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        channel = sess.channel_session()?;
        channel
            .exec(
                &("sudo systemctl restart nitro-enclaves-allocator.service"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        println!("Nitro Enclave Service set up with cpus: {} and memory: {}", v_cpus-2, mem-2048);

        channel = sess.channel_session()?;
        channel
            .exec(
                &("wget -O enclave.eif ".to_owned() + url),
            )?;
        let _ = channel.read_to_string(&mut s);
        let _ = channel.wait_close();
        println!("{}", s);

        channel = sess.channel_session()?;
        channel
            .exec(
                &("sudo iptables -A PREROUTING -t nat -p tcp --dport 80 -i ens5 -j REDIRECT --to-port 1200"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        channel = sess.channel_session()?;
        channel
            .exec(
                &("sudo iptables -A PREROUTING -t nat -p tcp --dport 443 -i ens5 -j REDIRECT --to-port 1200"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        channel = sess.channel_session()?;
        channel
            .exec(
                &("sudo iptables -A PREROUTING -t nat -p tcp --dport 1025:65535 -i ens5 -j REDIRECT --to-port 1200"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        channel = sess.channel_session()?;
        channel
            .exec(
                &("nitro-cli run-enclave --cpu-count ".to_owned() + &((v_cpus-2).to_string()) + " --memory " + &((mem-2200).to_string()) +" --eif-path enclave.eif --enclave-cid 88"),
            )?;

        let _ = channel.read_to_string(&mut s);
        println!("{}", s);
        let _ = channel.wait_close();

        println!("Enclave running");
        Ok(())
    }

    /* AWS EC2 UTILITY */

    pub async fn get_instance_ip(&self, instance_id: String) -> String {
        let resp = self.client
            .describe_instances()
            .instance_ids(instance_id.to_string())
            .send()
            .await;

        match resp {
            Ok(res) => {
                let reservations = res.reservations();
                if reservations.is_none() {
                    return String::new();
                }
                for reservation in reservations.unwrap() {
                    let instances = reservation.instances();
                    if instances.is_none() {
                        continue;
                    }
                    for instance in instances.unwrap() {
                        let ip = instance.public_ip_address();
                        if ip.is_some() {
                            return ip.unwrap().to_string();
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }

        return String::new();
    }

    pub async fn launch_instance(&self, job: String, instance_type: aws_sdk_ec2::model::InstanceType, image_url: &str, architecture: String) -> Result<String, Box<dyn Error + Send + Sync>> {
        let mut size: i64 = 0;
        let req_client = reqwest::Client::builder()
                .no_gzip()
                .build();
        match req_client {
            Ok(req_client) => {
                let res = req_client.head(image_url).send().await;
                match res {
                    Ok(res) => {
                        let content_len = res.headers()["content-length"].to_str().unwrap_or_else(|e| {
                            println!("ERROR: failed to fetch eif file header, setting default 15 GBs, {}", e);
                            "0"
                        });
                        size = content_len.parse::<i64>().unwrap_or_else(|e| {
                            println!("ERROR: failed to fetch eif file header, setting default 15 GBs, {}", e);
                            0
                        }) / 1000000;
                    },
                    Err(e) => {
                        println!("ERROR: failed to fetch eif file header, setting default 15 GBs, {}", e);
                    }
                }

            },
            Err(e) => {
                println!("ERROR: failed to fetch eif file header, setting default 15 GBs, {}", e);
            }
        }


        println!("eif size: {} MB", size);
        let size = size / 1000;
        let mut sdd = 15;
        if size > sdd {
            sdd = size + 10;
        }


        let (x86_ami, arm_ami) = self.get_amis().await;
        if x86_ami == String::new() || arm_ami == String::new() {
            println!("ERROR: AMI's not found");
            return Err("AMI's not found".into());
        }
        let mut instance_ami = x86_ami;
        if architecture == "arm64".to_string() {
            instance_ami = arm_ami;
        }

        let enclave_options = aws_sdk_ec2::model::EnclaveOptionsRequest::builder()
            .set_enabled(Some(true))
            .build();
        let ebs = aws_sdk_ec2::model::EbsBlockDevice::builder()
            .volume_size(sdd as i32)
            .build();
        let block_device_mapping = aws_sdk_ec2::model::BlockDeviceMapping::builder()
            .set_device_name(Some("/dev/sda1".to_string()))
            .set_ebs(Some(ebs))
            .build();
        let name_tag = aws_sdk_ec2::model::Tag::builder()
            .set_key(Some("Name".to_string()))
            .set_value(Some("JobRunner".to_string()))
            .build();
        let managed_tag = aws_sdk_ec2::model::Tag::builder()
            .set_key(Some("managedBy".to_string()))
            .set_value(Some("marlin".to_string()))
            .build();
        let project_tag = aws_sdk_ec2::model::Tag::builder()
            .set_key(Some("project".to_string()))
            .set_value(Some("oyster".to_string()))
            .build();
        let job_tag = aws_sdk_ec2::model::Tag::builder()
            .set_key(Some("jobId".to_string()))
            .set_value(Some(job))
            .build();
        let tags = aws_sdk_ec2::model::TagSpecification::builder()
            .set_resource_type(Some(aws_sdk_ec2::model::ResourceType::Instance))
            .tags(name_tag)
            .tags(managed_tag)
            .tags(job_tag)
            .tags(project_tag)
            .build();
        let subnet = self.get_subnet().await;
        let sec_group = self.get_security_group().await;


        let resp = self.client
            .run_instances()
            .set_image_id(Some(instance_ami))
            .set_instance_type(Some(instance_type))
            .set_key_name(Some(self.key_name.clone()))
            .set_min_count(Some(1))
            .set_max_count(Some(1))
            .set_enclave_options(Some(enclave_options))
            .block_device_mappings(block_device_mapping)
            .tag_specifications(tags)
            .security_group_ids(sec_group)
            .subnet_id(subnet)
            .send()
            .await;

        match resp {
            Ok(res) => {
                let instances = res.instances();
                if instances.is_none() {
                    println!("ERROR: instance launch failed");
                    return Err("Instance launch fail".into());
                }
                for instance in instances.unwrap() {
                    let id = instance.instance_id();
                    if id.is_none() {
                        println!("ERROR: error fetching instance id");
                        return Err("Instance launch fail".into());
                    }
                    println!(
                        "Instance launched - ID: {}",
                        id.unwrap()
                    );
                    return Ok(id.unwrap().to_string());
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
                return Err("Instance launch fail".into());
            }
        }

        return Err("Instance launch fail".into());
    }

    async fn terminate_instance(&self, instance_id: &String) -> Result<(), Box<dyn Error + Send + Sync>> {
        let _resp = self.client
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await?;

        // match resp {
        //     Ok(_) => {
        //         println!("Instance terminated");
        //     }
        //     Err(SdkError::ServiceError { err, .. }) => {
        //         if err.code().unwrap() == "InvalidInstanceID.NotFound" {
        //             println!("Instance not found")
        //         }
        //     }
        //     Err(e) => {
        //         panic!("ERROR: {}", e.to_string());
        //     }
        // }

        Ok(())
    }

    async fn get_amis(&self) -> (String, String) {
        let mut arm_ami = String::new();
        let mut x86_ami = String::new();

        let filter = aws_sdk_ec2::model::Filter::builder()
            .name("tag:project")
            .values("oyster")
            .build();

        let resp = self.client
                .describe_images()
                .owners("self")
                .filters(filter)
                .send()
                .await;

        match resp {
            Ok(res) => {
                let images = res.images();
                if images.is_none() {

                    return (x86_ami, arm_ami);
                }
                for image in images.unwrap() {
                    let image_name = image.name();
                    let image_id = image.image_id();
                    if image_name.is_none() || image_id.is_none(){
                        continue;
                    }
                    if "MarlinLauncherx86_64" == image_name.unwrap() {
                        println!("x86_64 ami: {}", image_id.unwrap());
                        x86_ami = image_id.unwrap().to_string();
                    } else if "MarlinLauncherARM64" == image_name.unwrap() {
                        println!("arm64 ami: {}", image_id.unwrap());
                        arm_ami = image_id.unwrap().to_string();
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }
        return (x86_ami, arm_ami);
    }

    pub async fn get_security_group(&self) -> String {
        let sec_group = String::new();
        let filter = aws_sdk_ec2::model::Filter::builder()
            .name("tag:project")
            .values("oyster")
            .build();

        let resp = self.client
            .describe_security_groups()
            .filters(filter)
            .send()
            .await;

        match resp {
            Ok(res) => {
                let groups = res.security_groups();
                if groups.is_none() {
                    println!("WARNING: oyster security groups not found");
                    return sec_group;
                }
                for group in groups.unwrap() {
                    let tags = group.tags();
                    if tags.is_none() {
                        continue;
                    }
                    for tagpair in  tags.unwrap() {
                        if "project" == tagpair.key().unwrap_or("") && "oyster" == tagpair.value().unwrap_or("") {

                            return group.group_id().unwrap_or("").to_string()
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }
        sec_group
    }

    pub async fn get_subnet(&self) -> String {
        let subnet = String::new();

        let filter = aws_sdk_ec2::model::Filter::builder()
            .name("tag:project")
            .values("oyster")
            .build();

        let resp = self.client
            .describe_subnets()
            .filters(filter)
            .send()
            .await;

        match resp {
            Ok(res) => {
                let subnets = res.subnets();
                if subnets.is_none() {
                    println!("WARNING: Oyster Subnet not found");
                    return subnet;
                }
                for subnet in subnets.unwrap() {
                    let tags = subnet.tags();
                    if tags.is_none() {
                        continue;
                    }
                    for tagpair in  tags.unwrap() {
                        if "project" == tagpair.key().unwrap_or("") && "oyster" == tagpair.value().unwrap_or("") {
                            println!("{}", subnet.subnet_id().unwrap_or(""));
                            return  subnet.subnet_id().unwrap_or("").to_string();
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }
        subnet
    }

    pub async fn get_job_instance(&self, job: String) -> (bool, String) {
        let resp = self.client.describe_instances().send().await;

        match resp {
            Ok(res) => {
                println!("Checking existing instance...");
                let reservations = res.reservations();
                if reservations.is_none() {
                    return (false, String::new());
                }
                for reservation in reservations.unwrap() {
                    let instances = reservation.instances();
                    if instances.is_none() {
                        continue;
                    }
                    for instance in instances.unwrap() {
                        let instance_id = instance.instance_id();
                        let tags = instance.tags();

                        if instance_id.is_none() || tags.is_none() {
                            continue;
                        }

                        for tag in tags.unwrap() {
                            if tag.key().unwrap_or("") == "jobId" && tag.value().unwrap_or("").to_string() == job
                            {
                                return (true, instance_id.unwrap().to_string());
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }
        return (false, String::new());
    }

    pub async fn spin_up(&self, image_url: &str, job: String, instance_type: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let ec2_type = aws_sdk_ec2::model::InstanceType::from_str(instance_type).unwrap_or_else(|e| {
            println!("ERROR: parsing instance_type, setting default, {}", e);
            return aws_sdk_ec2::model::InstanceType::C6aXlarge;
        });
        let resp = self.client
                .describe_instance_types()
                .instance_types(ec2_type)
                .send()
                .await;
        let mut architecture = "x86_64".to_string();
        let mut v_cpus: i32 = 4;
        let mut mem: i64 = 8192;
        match resp {
            Ok(resp) => {
                let instance_types = resp.instance_types();
                if instance_types.is_none() {
                    println!("ERROR: fetching instance info setting default");
                } else {
                    for instance in instance_types.unwrap() {
                        let processor_info = instance.processor_info();
                        if processor_info.is_some() {
                            let supported_architectures = processor_info.unwrap().supported_architectures();
                            if supported_architectures.is_some() {
                                for arch in supported_architectures.unwrap() {
                                    architecture = arch.as_str().to_string();
                                    println!("architecture: {}", arch.as_str());
                                    break;
                                }
                            }
                        }
                        let v_cpu_info = instance.v_cpu_info();
                        if v_cpu_info.is_some() {
                            let default_v_cpus = v_cpu_info.unwrap().default_v_cpus();
                            if default_v_cpus.is_some() {
                                v_cpus = default_v_cpus.unwrap();
                            }
                        }
                        println!("v_cpus: {}", v_cpus);
                        let mem_info = instance.memory_info();
                        if mem_info.is_some() {
                            let in_mib = mem_info.unwrap().size_in_mi_b();
                            if in_mib.is_some() {
                                mem = in_mib.unwrap();
                            }
                        }
                        println!("memory: {}", mem);
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {}", e.to_string());
            }
        }
        let instance_type = aws_sdk_ec2::model::InstanceType::from_str(instance_type).unwrap_or_else(|e| {
            println!("ERROR: parsing instance_type, setting default, {}", e);
            return aws_sdk_ec2::model::InstanceType::C6aXlarge;
        });
        let instance = self.launch_instance(job, instance_type, image_url, architecture).await;
        if let Err(err) = instance {
            println!("ERROR: error launching instance, {}", err);
            return Err("error launching instance".into());
        }
        let instance = instance.unwrap();
        sleep(Duration::from_secs(100)).await;

        let mut public_ip_address = self.get_instance_ip(instance.to_string()).await;
        if public_ip_address.len() == 0 {
            return Err("error fetching instance ip address".into());
        }
        public_ip_address.push_str(":22");
        let sess = self.ssh_connect(public_ip_address).await;
        match sess {
            Ok(r) => {
                let res = self.run_enclave(&r, image_url, v_cpus, mem).await;
                match res {
                    Ok(_) => return Ok(instance),
                    Err(_) => Err("error running enclave".into()),
                }
            },
            Err(_) => {
                return Err("error establishing ssh connection".into());
            }
        }

    }

    pub async fn spin_down(&self, instance_id: &String) -> Result<(), Box<dyn Error + Send + Sync>>{
        let _ = self.terminate_instance(&instance_id).await?;
        Ok(())
    }

}

#[async_trait]
impl AwsManager for Aws {
    async fn spin_up(
        &self,
        eif_url: &str,
        job: String,
        instance_type: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let instance = self.spin_up(eif_url, job, instance_type).await?;
        Ok(instance)
    }

    async fn spin_down(
        &self,
        instance_id: &String
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        let _ = self.spin_down(instance_id).await?;
        Ok(true)
    }

    async fn get_job_instance(
        &self,
        job: String) -> Result<(bool, String), Box<dyn Error + Send + Sync>> {
        let (exist, instance) = self.get_job_instance(job).await;
        Ok((exist, instance))
    }
}
