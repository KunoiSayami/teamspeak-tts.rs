use tokio::sync::mpsc::Receiver;

//pub type OnceSender<T> = tokio::sync::oneshot::Sender<T>;
pub use rusty_leveldb::Result;

#[derive(Clone, Debug)]
pub struct ConnAgent(DatabaseHelper);

type KeyType = u64;

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
        Set(KeyType, Vec<u8>),
        #[ret(Option<Vec<u8>>)]
        Get(
            KeyType,
        ),
        #[ret(Result<()>)]
        Delete(KeyType),
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
                    let ret = db.put(&k.to_be_bytes(), &v);
                    sender.send(ret).ok();
                    db.flush()?;
                }
                DatabaseEvent::Get(k, sender) => {
                    sender.send(db.get(&k.to_be_bytes())).ok();
                }
                DatabaseEvent::Delete(k, sender) => {
                    sender.send(db.delete(&k.to_be_bytes())).ok();
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
    pub async fn set(&self, key: KeyType, value: Vec<u8>) -> anyhow::Result<Option<()>> {
        if value.is_empty() {
            return Ok(None);
        }
        self.0
            .set(key, value)
            .await
            .map_or(Ok(()), |v| v.map_err(anyhow::Error::from))?;
        Ok(Some(()))
    }

    #[cfg(test)]
    pub async fn delete(&self, key: KeyType) -> anyhow::Result<()> {
        self.0.delete(key).await;
        Ok(())
    }

    pub async fn get(&self, key: KeyType) -> Option<Vec<u8>> {
        let ret = self.0.get(key).await.flatten()?;
        if ret.is_empty() {
            return None;
        }
        Some(ret)
    }
}

#[cfg(test)]
mod test {

    use super::{ConnAgent, LevelDB};

    async fn async_test_leveldb(agent: ConnAgent) -> anyhow::Result<()> {
        let conn = agent.clone();
        conn.set(114514, "value".as_bytes().to_vec()).await?;
        assert_eq!(conn.get(114514).await, Some("value".as_bytes().to_vec()));
        conn.set(114514, "value".as_bytes().to_vec()).await?;
        conn.delete(114514).await?;
        assert_eq!(conn.get(114514).await, None);

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
