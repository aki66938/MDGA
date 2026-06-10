#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseConfig {
    pub path: String,
}

/// 判断数据库配置是否指向本地 SQLite 文件。
///
/// 输入数据库配置，输出路径是否具备 SQLite 文件后缀；本方法不创建数据库连接。
pub fn points_to_sqlite(config: &DatabaseConfig) -> bool {
    config.path.ends_with(".sqlite") || config.path.ends_with(".sqlite3") || config.path.ends_with(".db")
}
