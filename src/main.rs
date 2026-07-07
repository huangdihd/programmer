use crate::ui::app::App;

mod ui;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let terminal = ratatui::init();
    let result = App::new().await.run(terminal).await;
    ratatui::restore();
    result
}
