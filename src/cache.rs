use tokio::sync::mpsc::Receiver;

//pub type OnceSender<T> = tokio::sync::oneshot::Sender<T>;
pub use rusty_leveldb::Result;

#[derive(Clone, Debug)]
pub struct ConnAgent(DatabaseHelper);

impl From<DatabaseHelper> for ConnAgent {
    fn from(value: DatabaseHelper) -> Self {
        Self(value)
    }
}

pub struct LevelDB {
    conn: DatabaseHelper,
    handle: std::thread::JoinHandle<Result<()>>,
}

kstool_helper_generator::oneshot_helper! {

    pub enum DatabaseEvent {
        #[ret(Result<()>)]
        Set(String, Vec<u8>),
        #[ret(Option<Vec<u8>>)]
        Get(
            String,
        ),
        #[ret(Result<()>)]
        Delete(String),
        Exit,
    }
}

impl LevelDB {
    pub fn opt() -> rusty_leveldb::Options {
        rusty_leveldb::Options {
            create_if_missing: true,
            ..Default::default()
        }
    }

    pub fn new(file: String) -> (ConnAgent, Self) {
        Self::new_with_opt(file, Self::opt)
    }

    fn new_with_opt(file: String, opt_fn: fn() -> rusty_leveldb::Options) -> (ConnAgent, Self) {
        let (sender, receiver) = DatabaseHelper::new(2048);

        (
            sender.clone().into(),
            Self {
                conn: sender,
                handle: std::thread::Builder::new()
                    .name(String::from("LevelDB thread"))
                    .spawn(move || Self::run(&file, opt_fn, receiver))
                    .expect("Fail to spawn thread"),
            },
        )
    }

    pub fn run(
        file: &str,
        opt_fn: fn() -> rusty_leveldb::Options,
        mut recv: Receiver<DatabaseEvent>,
    ) -> Result<()> {
        let mut db = rusty_leveldb::DB::open(file, opt_fn())?;
        while let Some(event) = recv.blocking_recv() {
            match event {
                DatabaseEvent::Set(k, v, sender) => {
                    let ret = db.put(k.as_bytes(), &v);
                    sender.send(ret).ok();
                    db.flush()?;
                }
                DatabaseEvent::Get(k, sender) => {
                    sender.send(db.get(k.as_bytes())).ok();
                }
                DatabaseEvent::Delete(k, sender) => {
                    sender.send(db.delete(k.as_bytes())).ok();
                    db.flush()?;
                }
                DatabaseEvent::Exit => break,
            }
        }
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub async fn exit(&self) -> Option<()> {
        self.conn.exit().await
    }

    pub fn connect(leveldb: String) -> (Self, ConnAgent) {
        let (conn, db) = Self::new(leveldb);

        (db, conn)
    }

    pub async fn disconnect(self) -> anyhow::Result<()> {
        if self.exit().await.is_none() {
            return Ok(());
        }
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if self.is_finished() {
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("Not exit after 3 seconds"))
    }
}

impl ConnAgent {
    pub async fn set(&self, key: &str, value: Vec<u8>) -> anyhow::Result<Option<()>> {
        if key.len() > 25 && key.len() < 50 {
            log::trace!("Skip {} {key}", key.len());
            return Ok(None);
        }
        self.0
            .set(key.to_string(), value)
            .await
            .map_or(Ok(()), |v| v.map_err(anyhow::Error::from))?;
        Ok(Some(()))
    }

    #[cfg(test)]
    pub async fn delete(&self, key: String) -> anyhow::Result<()> {
        self.0.delete(key.to_string()).await;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.get(key.to_string()).await.flatten()
    }
}

#[cfg(test)]
mod test {

    use super::{ConnAgent, LevelDB};

    async fn async_test_leveldb(agent: ConnAgent) -> anyhow::Result<()> {
        let conn = agent.clone();
        conn.set("key", "value".as_bytes().to_vec()).await?;
        assert_eq!(conn.get("key").await, Some("value".as_bytes().to_vec()));
        conn.set("key", "value".as_bytes().to_vec()).await?;
        conn.delete("key".to_string()).await?;
        assert_eq!(conn.get("key").await, None);

        Ok(())
    }

    #[test]
    fn test_leveldb() {
        let (agent, db) = LevelDB::new_with_opt("db".to_string(), rusty_leveldb::in_memory);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async_test_leveldb(agent))
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(db.disconnect())
            .unwrap();
    }
}
