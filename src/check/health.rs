use std::{
    collections::HashMap,
    io,
    net::{SocketAddr, ToSocketAddrs},
    sync::RwLock,
    time::{Duration, Instant},
};

use lazy_static::lazy_static;
use tokio::net::TcpStream;

lazy_static! {
    static ref HEALTH_CHECK: RwLock<HealthCheck> = RwLock::new(HealthCheck::new(60, 3, 2));
}

/// 每个SocketAddr的记录值
struct HealthRecord {
    /// 最后的记录时间
    last_record: Instant,
    /// 失败的恢复时间
    fail_timeout: Duration,
    /// 当前连续失败的次数
    fall_times: usize,
    /// 当前连续成功的次数
    rise_times: usize,
    /// 当前的状态
    failed: bool,
}

impl HealthRecord {
    pub fn new(fail_timeout: usize) -> Self {
        Self {
            last_record: Instant::now(),
            fail_timeout: Duration::new(fail_timeout as u64, 0),
            fall_times: 0,
            rise_times: 0,
            failed: false,
        }
    }

    pub fn clear_status(&mut self) {
        self.fall_times = 0;
        self.rise_times = 0;
        self.failed = false;
    }
}

/// 健康检查的控制中心
pub struct HealthCheck {
    /// 健康检查的重置时间, 失败超过该时间会重新检查, 统一单位秒
    fail_timeout: usize,
    /// 最大失败次数, 一定时间内超过该次数认为不可访问
    max_fails: usize,
    /// 最小上线次数, 到达这个次数被认为存活
    min_rises: usize,
    /// 记录跟地址相关的信息
    health_map: HashMap<SocketAddr, HealthRecord>,
}

impl HealthCheck {
    pub fn new(fail_timeout: usize, max_fails: usize, min_rises: usize) -> Self {
        Self {
            fail_timeout,
            max_fails,
            min_rises,
            health_map: HashMap::new(),
        }
    }

    pub fn instance() -> &'static RwLock<HealthCheck> {
        &HEALTH_CHECK
    }

    /// 检测状态是否能连接
    pub fn is_fall_down(addr: &SocketAddr) -> bool {
        // 只读，获取读锁
        if let Ok(h) = HEALTH_CHECK.read() {
            if !h.health_map.contains_key(addr) {
                return false;
            }
            let value = h.health_map.get(&addr).unwrap();
            if Instant::now().duration_since(value.last_record) > value.fail_timeout {
                return false;
            }
            h.health_map[addr].failed
        } else {
            false
        }
    }

    /// 失败时调用
    pub fn add_fall_down(addr: SocketAddr) {
        // 需要写入，获取写入锁
        if let Ok(mut h) = HEALTH_CHECK.write() {
            if !h.health_map.contains_key(&addr) {
                let mut health = HealthRecord::new(h.fail_timeout);
                health.fall_times = 1;
                h.health_map.insert(addr, health);
            } else {
                let max_fails = h.max_fails;
                let value = h.health_map.get_mut(&addr).unwrap();
                // 超出最大的失败时长，重新计算状态
                if Instant::now().duration_since(value.last_record) > value.fail_timeout {
                    value.clear_status();
                }
                value.last_record = Instant::now();
                value.fall_times += 1;
                value.rise_times = 0;

                if value.fall_times >= max_fails {
                    value.failed = true;
                }
            }
        }
    }

    /// 成功时调用
    pub fn add_rise_up(addr: SocketAddr) {
        // 需要写入，获取写入锁
        if let Ok(mut h) = HEALTH_CHECK.write() {
            if !h.health_map.contains_key(&addr) {
                let mut health = HealthRecord::new(h.fail_timeout);
                health.rise_times = 1;
                h.health_map.insert(addr, health);
            } else {
                let min_rises = h.min_rises;
                let value = h.health_map.get_mut(&addr).unwrap();
                // 超出最大的失败时长，重新计算状态
                if Instant::now().duration_since(value.last_record) > value.fail_timeout {
                    value.clear_status();
                }
                value.last_record = Instant::now();
                value.rise_times += 1;
                value.fall_times = 0;

                if value.rise_times >= min_rises {
                    value.failed = false;
                }
            }
        }
    }

    // 将TcpStream::connect函数替换成这个函数，将自动启用被动健康检查
    pub async fn connect<A>(addr: &A) -> io::Result<TcpStream>
    where
        A: ToSocketAddrs,
    {
        let addrs = addr.to_socket_addrs()?;
        let mut last_err = None;

        for addr in addrs {
            // 健康检查失败，直接返回错误
            if Self::is_fall_down(&addr) {
                last_err = Some(io::Error::new(io::ErrorKind::Other, "health check falldown"));
            } else {
                match TcpStream::connect(&addr).await {
                    Ok(stream) => 
                    {
                        Self::add_rise_up(addr);
                        return Ok(stream)
                    },
                    Err(e) => {
                        Self::add_fall_down(addr);
                        last_err = Some(e)
                    },
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }
}