use std::sync::mpsc as sync_mpsc;
use tokio::sync::mpsc as async_mpsc;

/// 对讲机：可以说（发送 A），可以听（接收 B）
pub struct Intercom<Send, Recv> {
    tx: sync_mpsc::Sender<Send>,
    rx: sync_mpsc::Receiver<Recv>,
}

impl<Send, Recv> Intercom<Send, Recv> {
    /// 发送消息给对方
    pub fn send(&self, msg: Send) -> Result<(), sync_mpsc::SendError<Send>> {
        self.tx.send(msg)
    }

    /// 阻塞接收消息
    pub fn recv(&self) -> Result<Recv, sync_mpsc::RecvError> {
        self.rx.recv()
    }

    /// 非阻塞尝试接收
    pub fn try_recv(&self) -> Result<Recv, sync_mpsc::TryRecvError> {
        self.rx.try_recv()
    }
}

/// 创建一对互连的对讲机
pub fn make_intercom_pair<A, B>() -> (Intercom<A, B>, Intercom<B, A>) {
    let (tx1, rx1) = sync_mpsc::channel();
    let (tx2, rx2) = sync_mpsc::channel();

    let radio1 = Intercom { tx: tx1, rx: rx2 };
    let radio2 = Intercom { tx: tx2, rx: rx1 };

    (radio1, radio2)
}

/// 异步对讲机（未拆分）
pub struct AsyncIntercom<Send, Recv> {
    tx: async_mpsc::Sender<Send>,
    rx: async_mpsc::Receiver<Recv>,
}

/// 话筒：只负责发送
pub struct Speaker<Send> {
    tx: async_mpsc::Sender<Send>,
}

/// 听筒：只负责接收
pub struct Listener<Recv> {
    rx: async_mpsc::Receiver<Recv>,
}

// Speaker 可克隆，允许多个任务共享发送能力
impl<Send> Clone for Speaker<Send> {
    fn clone(&self) -> Self {
        Speaker {
            tx: self.tx.clone(),
        }
    }
}

impl<Send> Speaker<Send> {
    pub async fn send(&self, msg: Send) -> Result<(), async_mpsc::error::SendError<Send>> {
        self.tx.send(msg).await
    }

    pub fn try_send(&self, msg: Send) -> Result<(), async_mpsc::error::TrySendError<Send>> {
        self.tx.try_send(msg)
    }
}

impl<Recv> Listener<Recv> {
    pub async fn recv(&mut self) -> Option<Recv> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Recv, async_mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }
}

impl<Send, Recv> AsyncIntercom<Send, Recv> {
    pub async fn send(&self, msg: Send) -> Result<(), async_mpsc::error::SendError<Send>> {
        self.tx.send(msg).await
    }

    pub async fn recv(&mut self) -> Option<Recv> {
        self.rx.recv().await
    }

    /// 将对讲机拆成话筒和听筒，可交给不同任务
    pub fn split(self) -> (Speaker<Send>, Listener<Recv>) {
        let speaker = Speaker { tx: self.tx };
        let listener = Listener { rx: self.rx };
        (speaker, listener)
    }
}

/// 创建一对异步对讲机
pub fn make_async_intercom_pair<A, B>(buffer: usize) -> (AsyncIntercom<A, B>, AsyncIntercom<B, A>) {
    let (tx1, rx1) = async_mpsc::channel(buffer);
    let (tx2, rx2) = async_mpsc::channel(buffer);

    let radio1 = AsyncIntercom { tx: tx1, rx: rx2 };
    let radio2 = AsyncIntercom { tx: tx2, rx: rx1 };

    (radio1, radio2)
}
