use chrono::Utc;
use scylla::prepared_statement::PreparedStatement;
use scylla::transport::query_result::FirstRowTypedError;
use scylla::{FromRow, FromUserType, IntoUserType, Session, SessionBuilder};
use serde::Serialize;

use super::err_models::VpError;
use super::p_models::UpdatePixel;

//ScyllaBuilder
pub struct ScyllaBuilder {
    session: Session,
    dim_mid: u32,
}
impl ScyllaBuilder {
    pub async fn try_init(scylla_url: &str, canvas_dim: u32) -> Result<Self, VpError> {
        let session = SessionBuilder::new().known_node(scylla_url).build().await?;
        let dim_mid = canvas_dim / 2;
        Ok(Self { session, dim_mid })
    }
    async fn init_table(&self) -> Result<(), VpError> {
        //Store Pixel Update of Each User
        //->used to check cooldown
        self.session.query("CREATE KEYSPACE IF NOT EXISTS opbnbplace WITH REPLICATION = {'class' : 'NetworkTopologyStrategy', 'replication_factor' : 1}", &[]).await?;
        //table to store User's last pixel placement
        self.session
        .query("CREATE TABLE IF NOT EXISTS opbnbplace.player (address text,x int,y int,color int,last_placed timestamp,PRIMARY KEY (address))", &[])
        .await?;

        //Store All Pixel data
        // UDT to store pixel_data
        self.session.query("CREATE TYPE IF NOT EXISTS opbnbplace.pixel_data (address text,color int,last_placed timestamp)",&[]).await?;
        //table to store all pixel update data in canvas
        // Divide the canvas into 4 parts
        //       ---------------
        //       |      |      |
        //       |   1  |  2   |
        //       |------|------|
        //       |   3  |  4   |
        //       |      |      |
        //       --------------
        // each part is row with pixel details as column of the form (x,y):pixel_data
        // where pixel_data is UDT defined above : ) .
        self.session.query("CREATE TABLE IF NOT EXISTS opbnbplace.canvas ( canvas_part text,x int ,y int,data frozen<pixel_data>,PRIMARY KEY (canvas_part,x,y))",&[]).await?;
        Ok(())
    }

    pub async fn try_build(self) -> Result<ScyllaManager, VpError> {
        self.init_table().await?;
        let insert_user=self.session.prepare("INSERT INTO opbnbplace.player (address, x, y, color, last_placed) VALUES (?, ?, ?, ?, ?)").await?;
        let get_user = self
            .session
            .prepare(
                "SELECT address, x, y, color, last_placed FROM opbnbplace.player WHERE address = ?",
            )
            .await?;
        let insert_pixel = self
            .session
            .prepare("INSERT INTO opbnbplace.canvas (canvas_part,x,y,data) VALUES (?, ?, ?, ?)")
            .await?;
        let get_pixel = self
            .session
            .prepare("SELECT data FROM opbnbplace.canvas WHERE canvas_part = ? AND x=? AND y=?")
            .await?;
        Ok(ScyllaManager {
            session: self.session,
            dim_mid: self.dim_mid,
            insert_user,
            get_user,
            insert_pixel,
            get_pixel,
            canvas_part: ["v_part1", "v_part2", "v_part3", "v_part4"],
        })
    }
}

//ScyllaDb Manager
pub struct ScyllaManager {
    session: Session,
    dim_mid: u32,
    insert_user: PreparedStatement,
    get_user: PreparedStatement,
    insert_pixel: PreparedStatement,
    get_pixel: PreparedStatement,
    canvas_part: [&'static str; 4],
}
impl ScyllaManager {
    pub async fn get_user(&self, address: &String) -> Result<UserDetails, VpError> {
        let rows = self.session.execute(&self.get_user, (address,)).await?;
        let res = rows.first_row_typed::<UserDetails>();
        match res {
            Ok(res) => Ok(res),
            Err(FirstRowTypedError::RowsEmpty) => Err(VpError::InvalidUser),
            Err(e) => Err(VpError::ScyllaTypeErr(e)),
        }
    }
    pub async fn update_db(&self, req: &UpdatePixel) -> Result<(), VpError> {
        let (ix, iy) = (i32::try_from(req.loc.x)?, i32::try_from(req.loc.y)?);
        // infallible :)
        let color = i32::try_from(req.color).unwrap();
        //already checked in handler
        let address = req.address.as_ref().ok_or_else(|| VpError::InvalidUser)?;
        let last_placed = Utc::now().timestamp();

        // add user update
        let user_update = self
            .session
            .execute(&self.insert_user, (address, ix, iy, color, last_placed));

        // add  pixel update
        let pindex = match (req.loc.x <= self.dim_mid, req.loc.y <= self.dim_mid) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        };
        let pixel_data = PixelData {
            address: address.to_string(),
            color,
            last_placed,
        };
        let pixel_update = self.session.execute(
            &self.insert_pixel,
            (self.canvas_part[pindex], ix, iy, pixel_data),
        );
        tokio::try_join!(user_update, pixel_update)?;
        Ok(())
    }
    pub async fn get_pixel(&self, x: u32, y: u32) -> Result<PixelData, VpError> {
        let ix = i32::try_from(x)?;
        let iy = i32::try_from(y)?;
        let pindex = match (x <= self.dim_mid, y <= self.dim_mid) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        };
        let rows = self
            .session
            .execute(&self.get_pixel, (self.canvas_part[pindex], ix, iy))
            .await?;
        let res = rows.first_row_typed::<(PixelData,)>();
        match res {
            Ok(res) => Ok(res.0),
            Err(FirstRowTypedError::RowsEmpty) => Err(VpError::NoPixelData),
            Err(e) => Err(VpError::ScyllaTypeErr(e)),
        }
    }
}

//ScyllaDb RowData
#[derive(FromRow)]
pub struct UserDetails {
    pub address: String,
    pub x: i32,     //u32 aan sherikkum , but CQL derive does'nt support : )
    pub y: i32,     // same as above : )
    pub color: i32, // sherikkum u8
    pub last_placed: i64,
}

#[derive(IntoUserType, FromUserType, Serialize)]
pub struct PixelData {
    pub address: String,
    pub color: i32,
    pub last_placed: i64,
}
