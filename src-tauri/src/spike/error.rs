use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Credentials,
    RateLimit,
    Network,
    Microphone,
    Protocol,
    NoFinal,
    FinalTimeout,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credentials => "credentials",
            Self::RateLimit => "rate_limit",
            Self::Network => "network",
            Self::Microphone => "microphone",
            Self::Protocol => "protocol",
            Self::NoFinal => "no_final",
            Self::FinalTimeout => "final_timeout",
        }
    }
}

#[derive(Debug, Error)]
pub enum SpikeError {
    #[error("未配置本地凭据")]
    CredentialsMissing,
    #[error("凭据或资源被拒绝 ({0})")]
    AuthRejected(u16),
    #[error("请求被限流 ({0})")]
    RateLimited(u16),
    #[error("网络连接失败")]
    Network,
    #[error("系统默认麦克风不可用")]
    MicrophoneUnavailable,
    #[error("麦克风采集失败")]
    MicrophoneFailed,
    #[error("服务端协议帧无效")]
    Protocol,
    #[error("服务端未返回最终结果")]
    NoFinalResult,
    #[error("等待最终结果超时")]
    FinalResultTimeout,
    #[error("服务端返回识别错误 ({0})")]
    ServerError(u32),
    #[error("命令行参数无效")]
    InvalidArguments,
}

impl SpikeError {
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::CredentialsMissing | Self::AuthRejected(_) => ErrorClass::Credentials,
            Self::RateLimited(_) => ErrorClass::RateLimit,
            Self::Network => ErrorClass::Network,
            Self::MicrophoneUnavailable | Self::MicrophoneFailed => ErrorClass::Microphone,
            Self::Protocol | Self::ServerError(_) | Self::InvalidArguments => ErrorClass::Protocol,
            Self::NoFinalResult => ErrorClass::NoFinal,
            Self::FinalResultTimeout => ErrorClass::FinalTimeout,
        }
    }
}
