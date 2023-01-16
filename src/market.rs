use ethers::abi::AbiDecode;
use ethers::prelude::*;
use ethers::utils::keccak256;
use serde_json::Value;
use std::error::Error;
use std::time::SystemTime;
use tokio::time::sleep;
use tokio::time::Duration;
use tokio_stream::Stream;
use std::fs;
use whoami;
use std::str::FromStr;
use async_trait::async_trait;

use crate::launcher;
// Basic architecture:
// One future listening to new jobs
// Each job has its own future managing its lifetime

pub struct JobsService {}

#[async_trait]
pub trait AwsManager {
    async fn spin_up(
        &self,
        eif_url: &str,
        job: String,
        instance_type: &str) -> Result<String, Box<dyn Error + Send + Sync>>;
    
    async fn spin_down(
        &self,
        instance_id: &String
    ) -> Result<bool, Box<dyn Error + Send + Sync>>;

    async fn get_job_instance(
        &self,
        job: String
    ) -> Result<(bool, String), Box<dyn Error + Send + Sync>>;
}

// pub struct JobItem {}

// impl Stream for JobItem{
//     type Item = (H256, bool);

//     fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
//         -> Poll<Option<(H256, bool)>>
//     {
//         return Poll::Ready(None);
//     }
// }

// pub struct LogItem {}

// impl Stream for LogItem {
//     type Item = Log;

//     fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
//         -> Poll<Option<Log>>
//     {
        
//         return Poll::Ready(None);        
//     }
// }
#[async_trait]
pub trait Logger<'a> {
    async fn new_jobs(
        &self,
        client: & Provider<Ws>
    ) -> Result<Box<dyn Stream<Item = (H256, bool)>>, Box<dyn Error + Send + Sync>>;

    async fn job_logs(
        &self,
        client: &Provider<Ws>,
        job: H256
    ) -> Result<Box<dyn Stream<Item = Log> + Send>, Box<dyn Error + Send + Sync>>;
}

#[derive(Clone)]
pub struct RealLogger {}

#[async_trait]
impl<'a> Logger<'a> for RealLogger {
    async fn new_jobs(
        &self,
        client: & Provider<Ws>
    ) -> Result<Box<dyn Stream<Item = (H256, bool)>>, Box<dyn Error + Send + Sync>> {
        // let js = JobsService{};
        let res = JobsService::new_jobs(client).await;
        res
    }

    async fn job_logs(
        &self,
        client: &Provider<Ws>,
        job: H256
    ) -> Result<Box<dyn Stream<Item = Log> + Send>, Box<dyn Error + Send + Sync>> {
        // let js = JobsService{};
        let res = JobsService::job_logs(client, job).await;
        res
    }
}

#[derive(Clone)]
pub struct RealAws {}

#[async_trait]
impl AwsManager for RealAws {
    async fn spin_up(
        &self,
        eif_url: &str,
        job: String,
        instance_type: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
            let instance = launcher::spin_up(eif_url, job, instance_type).await;
            Ok(instance)
    }

    async fn spin_down(
        &self,
        instance_id: &String
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        launcher::spin_down(instance_id).await;
        Ok(true)
    }

    async fn get_job_instance(
        &self,
        job: String) -> Result<(bool, String), Box<dyn Error + Send + Sync>> {
        let (exist, instance) = launcher::get_job_instance(job).await;
        Ok((exist, instance))
    }
}

impl JobsService {
    pub async fn run(
        aws_manager_impl: impl AwsManager + Send + Sync + Clone + 'static,
        logger_impl: impl Logger<'_> + Send + Sync + Clone + 'static,
        url: String) {
        let mut backoff = 1;

        // connection level loop
        // start from scratch in case of connection errors
        // trying to implicitly resume connections or event streams can cause issues
        // since subscriptions are stateful
        loop {
            println!("main: Connecting to RPC endpoint...");
            let res = Provider::<Ws>::connect(url.clone()).await;
            if let Err(err) = res {
                // exponential backoff on connection errors
                println!("main: Connection error: {}", err);
                sleep(Duration::from_secs(backoff)).await;
                backoff *= 2;
                if backoff > 128 {
                    backoff = 128;
                }
                continue;
            }
            backoff = 1;
            println!("main: Connected to RPC endpoint");

            let client = res.unwrap();
            // let res = Self::new_jobs(&client).await;
            let res = logger_impl.new_jobs(&client).await;
            if let Err(err) = res {
                println!("main: Subscribe error: {}", err);
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            let mut job_stream = Box::into_pin(res.unwrap());
            while let Some((job, removed)) = job_stream.next().await {
                println!("main: New job: {}, {}", job, removed);
                tokio::spawn(Self::job_manager(aws_manager_impl.clone(), logger_impl.clone(), url.clone(), job));
            }

            println!("main: Job stream ended");
        }
    }

    async fn new_jobs(
        client: &Provider<Ws>,
    ) -> Result<Box<dyn Stream<Item = (H256, bool)> + '_>, Box<dyn Error + Send + Sync + '_>> {
        // TODO: Filter by contract and provider address
        let event_filter =
            Filter::new()
                .address(ValueOrArray::Value("0x3FA4718a2fd55297CD866E5a0dE6Bc75E2b777d1".parse::<Address>()?))
                .select(0..)
                .topic0(ValueOrArray::Array(vec![H256::from(keccak256(
                    "JobOpened(bytes32,string,address,address,uint256,uint256,uint256)",
                ))]));

        // register subscription
        let stream = client.subscribe_logs(&event_filter).await?;

        Ok(Box::new(stream.map(|item| (item.topics[1], item.removed.unwrap_or(false)))))
    }

    

    // manage the complete lifecycle of a job
    async fn job_manager(aws_manager_impl: impl AwsManager + Send + Sync + Clone, logger_impl: impl Logger<'_> + Send + Sync + Send, url: String, job: H256) {
        let mut backoff = 1;

        // connection level loop
        // start from scratch in case of connection errors
        // trying to implicitly resume connections or event streams can cause issues
        // since subscriptions are stateful
        'main: loop {
            println!("job {}: Connecting to RPC endpoint...", job);
            let res = Provider::<Ws>::connect(url.clone()).await;
            if let Err(err) = res {
                // exponential backoff on connection errors
                println!("job {}: Connection error: {}", job, err);
                sleep(Duration::from_secs(backoff)).await;
                backoff *= 2;
                if backoff > 128 {
                    backoff = 128;
                }
                continue;
            }
            backoff = 1;
            println!("job {}: Connected to RPC endpoint", job);

            let client = res.unwrap();
            // let res = Self::job_logs(&client, job).await;
            let res = logger_impl.job_logs(&client, job).await;
            if let Err(err) = res {
                println!("job {}: Subscribe error: {}", job, err);
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            // events
            let JOB_OPENED = H256::from(keccak256(
                "JobOpened(bytes32,string,address,address,uint256,uint256,uint256)",
            ));
            let JOB_SETTLED = H256::from(keccak256("JobSettled(bytes32,uint256,uint256)"));
            let JOB_CLOSED = H256::from(keccak256("JobClosed(bytes32)"));
            let JOB_DEPOSITED = H256::from(keccak256("JobDeposited(bytes32,address,uint256)"));
            let JOB_WITHDREW = H256::from(keccak256("JobWithdrew(bytes32,address,uint256)"));
            let JOB_REVISED_RATE = H256::from(keccak256("JobRevisedRate(bytes32,uint256)"));
            let LOCK_CREATED =
                H256::from(keccak256("LockCreated(bytes32,bytes32,uint256,uint256)"));
            let LOCK_DELETED = H256::from(keccak256("LockDeleted(bytes32,bytes32,uint256)"));

            // solvency metrics
            // default of 60s
            let mut balance = U256::from(60);
            let mut last_settled = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap();
            let mut rate = U256::one();
            let mut original_rate = U256::one();
            let mut instance_id = String::new();
            let mut job_stream = Box::into_pin(res.unwrap());
            let mut min_rate = U256::one();
            let mut eif_url = String::new();
            let mut instance_type = "c6a.xlarge".to_string();
            'event: loop {
                // compute time to insolvency
                let now_ts = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap();
                fn sat_convert(n: U256) -> u64 {
                    let lowu64 = n.low_u64();
                    if n == lowu64.into() {
                        lowu64
                    } else {
                        u64::MAX
                    }
                }

                // NOTE: should add margin for node to spin down?
                let insolvency_duration = if rate == U256::zero() {
                    Duration::from_secs(0)
                } else {
                    Duration::from_secs(sat_convert(balance / rate))
                        .saturating_sub(now_ts.saturating_sub(last_settled))
                };
                println!(
                    "job {}: Insolvency after: {}",
                    job,
                    insolvency_duration.as_secs()
                );

                tokio::select! {
                    // insolvency check
                    () = sleep(insolvency_duration) => {
                        // TODO: spin down instance
                        if instance_id != String::new() {
                            aws_manager_impl.spin_down(&instance_id).await;
                        }
                        println!("job {}: INSOLVENCY: Spinning down instance", job);

                        // exit fully
                        break 'main;
                    }
                    log = job_stream.next() => {
                        if log.is_none() { break 'event; }
                        let log = log.unwrap();
                        println!("job {}: New log: {}, {}", job, log.topics[0], log.data);

                        if log.topics[0] == JOB_OPENED {
                            // decode
                            if let Ok((metadata, _rate, _balance, timestamp)) = <(String, U256, U256, U256)>::decode(&log.data) {
                                // update solvency metrics
                                balance = _balance;
                                rate = _rate;
                                original_rate = _rate;
                                last_settled = Duration::from_secs(timestamp.low_u64());
                                println!("job {}: OPENED: metadata: {}, rate: {}, balance: {}, timestamp: {}", job, metadata, rate, balance, last_settled.as_secs());
                                let v: Value = serde_json::from_str(&metadata).expect("JSON was not well-formatted");
                                // TODO: spin up instance
                                
                                let r = v["instance"].as_str();
                                eif_url = v["url"].as_str().unwrap().to_string();
                                match r {
                                    Some(t) => {
                                        instance_type = t.to_string();
                                        println!("Instance type set: {}", instance_type);
                                    }
                                    None => {
                                        println!("Instance type not set, using default");
                                    }
                                }
                                let file_path = "/home/".to_owned() + &whoami::username() +"/.marlin/rates.txt";
                                let contents = fs::read_to_string(file_path)
                                .expect("Cannot read in rates file").to_string();

                                let lines = contents.lines();
                                for line in lines {
                                    if line.contains(&instance_type) {
                                        let rate_card: Vec<String> = line.split(":").map(String::from).collect();
                                        min_rate = U256::from_str(rate_card[1].as_str()).unwrap();
                                    }
                                }
                                println!("MIN RATE for {} instance is {}", instance_type, min_rate);
                                let (exist, instance) = aws_manager_impl.get_job_instance(job.to_string()).await.unwrap();
                                if exist {
                                    instance_id = instance;
                                    println!("Found, instance id: {}", instance_id);
                                    if rate < min_rate {
                                        println!("Rate below minimum, shutting down instance");
                                        aws_manager_impl.spin_down(&instance_id).await;
                                    }
                                } else {
                                    if rate >= min_rate {
                                        instance_id = aws_manager_impl.spin_up(eif_url.as_str(), job.to_string(), instance_type.as_str()).await.unwrap();
                                    } else {
                                        println!("Rate below minimum, aborting launch.");
                                    }
                                    
                                }

                                println!("job {}: OPENED: Spun up instance", job);
                            } else {
                                println!("job {}: OPENED: Decode failure: {}", job, log.data);
                            }
                        } else if log.topics[0] == JOB_SETTLED {
                            // decode
                            if let Ok((amount, timestamp)) = <(U256, U256)>::decode(&log.data) {
                                // update solvency metrics
                                balance -= amount;
                                last_settled = Duration::from_secs(timestamp.low_u64());
                                println!("job {}: SETTLED: amount: {}, rate: {}, balance: {}, timestamp: {}", job, amount, rate, balance, last_settled.as_secs());
                            } else {
                                println!("job {}: SETTLED: Decode failure: {}", job, log.data);
                            }
                        } else if log.topics[0] == JOB_CLOSED {
                            // TODO: spin down instance
                            aws_manager_impl.spin_down(&instance_id).await;
                            println!("job {}: CLOSED: Spinning down instance", job);

                            // exit fully
                            println!("job {}: CLOSED", job);
                            break 'main;
                        } else if log.topics[0] == JOB_DEPOSITED {
                            // decode
                            if let Ok(amount) = U256::decode(&log.data) {
                                // update solvency metrics
                                balance += amount;
                                println!("job {}: DEPOSITED: amount: {}, rate: {}, balance: {}, timestamp: {}", job, amount, rate, balance, last_settled.as_secs());
                            } else {
                                println!("job {}: DEPOSITED: Decode failure: {}", job, log.data);
                            }
                        } else if log.topics[0] == JOB_WITHDREW {
                            // decode
                            if let Ok(amount) = U256::decode(&log.data) {
                                // update solvency metrics
                                balance -= amount;
                                println!("job {}: WITHDREW: amount: {}, rate: {}, balance: {}, timestamp: {}", job, amount, rate, balance, last_settled.as_secs());
                            } else {
                                println!("job {}: WITHDREW: Decode failure: {}", job, log.data);
                            }
                        } else if log.topics[0] == JOB_REVISED_RATE {
                            // update solvency metrics
                            
                            original_rate = rate;
                            if rate >= min_rate {
                                let (exist, instance) = aws_manager_impl.get_job_instance(job.to_string()).await.unwrap();
                                if exist {
                                    instance_id = instance;
                                    println!("Found, instance id: {}", instance_id);
                                } else {
                                    instance_id = aws_manager_impl.spin_up(eif_url.as_str(), job.to_string(), instance_type.as_str()).await.unwrap();
                                }
                            }
                            println!("job {}: REVISED_RATE: rate: {}, balance: {}, timestamp: {}", job, rate, balance, last_settled.as_secs());
                        } else if log.topics[0] == LOCK_CREATED {
                            // decode
                            if let Ok((new_rate, _)) = <(U256, U256)>::decode(&log.data) {
                                // update solvency metrics
                                original_rate = rate;
                                rate = new_rate;
                                if rate < min_rate {
                                    aws_manager_impl.spin_down(&instance_id).await;
                                    println!("Revised job rate below min rate, shutting down");
                                }
                                println!("job {}: LOCK_CREATED: original_rate: {}, rate: {}, balance: {}, timestamp: {}", job, original_rate, rate, balance, last_settled.as_secs());
                            } else {
                                println!("job {}: LOCK_CREATED: Decode failure: {}", job, log.data);
                            }
                        } else if log.topics[0] == LOCK_DELETED {
                            // update solvency metrics
                            original_rate = rate;
                            println!("job {}: LOCK_CREATED: original_rate: {}, rate: {}, balance: {}, timestamp: {}", job, original_rate, rate, balance, last_settled.as_secs());
                        } else {
                            println!("job {}: Unknown event: {}", job, log.topics[0]);
                        }
                    }
                }
            }

            println!("job {}: Job stream ended", job);
        }
    }

    async fn job_logs(
        client: &Provider<Ws>,
        job: H256,
    ) -> Result<Box<dyn Stream<Item = Log> + Send + '_>, Box<dyn Error + Send + Sync + '_>> {
        // TODO: Filter by contract and job
        let event_filter = Filter::new().select(0..)
            .address(ValueOrArray::Value("0x3FA4718a2fd55297CD866E5a0dE6Bc75E2b777d1".parse::<Address>()?))
            .topic0(ValueOrArray::Array(vec![
            H256::from(keccak256(
                "JobOpened(bytes32,string,address,address,uint256,uint256,uint256)",
            )),
            H256::from(keccak256("JobSettled(bytes32,uint256,uint256)")),
            H256::from(keccak256("JobClosed(bytes32)")),
            H256::from(keccak256("JobDeposited(bytes32,address,uint256)")),
            H256::from(keccak256("JobWithdrew(bytes32,address,uint256)")),
            H256::from(keccak256("JobRevisedRate(bytes32,uint256)")),
            H256::from(keccak256("LockCreated(bytes32,bytes32,uint256,uint256)")),
            H256::from(keccak256("LockDeleted(bytes32,bytes32,uint256)")),
        ])).topic1(ValueOrArray::Value(job));

        // register subscription
        let stream = client.subscribe_logs(&event_filter).await?;

        Ok(Box::new(stream))
    }
}
