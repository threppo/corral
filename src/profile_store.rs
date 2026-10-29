//! Save integration for desk-setups-spec sections 3-5. No selection/scoring policy lives here.
use crate::{desk_setup::{self, Case, MonitorState}, model::Layout};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Switch { Unchanged, Selected(Case) }
pub fn switch_states(layout:&mut Layout,monitors:&[MonitorState],now:i64)->Switch {
    let before=layout.desk_setups.runtime.decision.clone();let d=desk_setup::evaluate(layout,monitors,now);
    if before==d {Switch::Unchanged}else{Switch::Selected(d.case)}
}
pub fn edited(layout:&mut Layout,now:i64,pending:bool)->bool {desk_setup::user_edit(layout,now,pending)}
