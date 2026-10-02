//! 分区树的**纯逻辑**：可见行、光标、展开状态、窗口。
//!
//! 只算不画 —— 画在 `control.rs` 的右栏里。拆出来是为了几个边界能离线钉死：
//! 「父分区自己也是一行」「没配过 / 配过各自的默认状态」「光标不许跑出可视区」。

use crate::api::area::ParentArea;

/// 树里的一行。
///
/// 父分区自己也是一行，这是**故意的**：Go 版用 tview 的 TreeView，上下键只停在
/// 「可选节点」上，父分区不可选就被整段跳过去（`↑↓` 从上一个父分区直接跳到子分区），
/// 用户报的现象是「分区没法选」。这里显式把它算成一行，`↑↓` 一定会停在它上面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// 顶上那一行「全部分区」
    All,
    /// 第 p 个父分区自己那一行
    Parent(usize),
    /// 第 p 个父分区的第 s 个子分区
    Sub(usize, usize),
}

pub struct AreaTree {
    parents: Vec<ParentArea>,
    /// 与 `parents` 一一对应
    expanded: Vec<bool>,
    /// 「全部分区」自己也能收起（收起后只剩它一行），默认展开
    root_expanded: bool,
    cursor: Row,
}

impl AreaTree {
    /// 建树，顺便把光标摆到该在的地方。
    ///
    /// - 没配过（`saved_area_id == 0`）：光标停在「全部分区」上，**所有父分区收起**
    /// - 配过：只展开**它所在的那个**父分区，光标落到那个子分区上
    ///
    /// 这两条跟「列表里的第一个」没有任何关系。Go 版拿 `SetExpanded(是不是第一项)`
    /// 当判据，结果每次进来都展开「网游」，用户配置里那个分区却躺在别的父分区里
    /// —— 界面看着一切正常，就是跟他配的东西对不上。
    ///
    /// 配过的分区在今天的表里没了（下线的分区）：退回「全部分区 + 全都收起」，
    /// 而不是随便展开一个。
    pub fn new(parents: Vec<ParentArea>, saved_area_id: i64) -> Self {
        let mut expanded = vec![false; parents.len()];
        let mut cursor = Row::All;
        if saved_area_id != 0
            && let Some((p, s)) = find_sub(&parents, saved_area_id)
        {
            expanded[p] = true;
            cursor = Row::Sub(p, s);
        }
        Self {
            parents,
            expanded,
            root_expanded: true,
            cursor,
        }
    }

    /// 现在屏幕从上到下到底是哪几行。
    pub fn visible(&self) -> Vec<Row> {
        let mut out = vec![Row::All];
        if !self.root_expanded {
            return out;
        }
        for (p, parent) in self.parents.iter().enumerate() {
            out.push(Row::Parent(p));
            if self.is_expanded(p) {
                for s in 0..parent.list.len() {
                    out.push(Row::Sub(p, s));
                }
            }
        }
        out
    }

    /// 每一行的字：缩进和展开标记都已经拼好（`▾` 展开 / `▸` 收起）。
    pub fn lines(&self) -> Vec<String> {
        self.visible().iter().map(|r| self.text(*r)).collect()
    }

    /// 一行的字。索引全走 `get`：这一层 panic 一次就是整屏消失
    /// （行号只由 `visible()` 产生，本身不会越界，但 `text` 是 pub 的，
    /// 以后有人从别处拼一个行号进来，宁可少一行字）。
    pub fn text(&self, row: Row) -> String {
        match row {
            Row::All => format!("{}全部分区", if self.root_expanded { "▾ " } else { "▸ " }),
            Row::Parent(p) => match self.parents.get(p) {
                // 一个子分区都没有的父分区不打展开标记：打了也点不开
                // （真接口里没见过，但空 `list` 是允许的），用户会以为键坏了。
                Some(parent) if parent.list.is_empty() => format!("  {}", parent.name),
                Some(parent) => format!(
                    "{}{}",
                    if self.is_expanded(p) { "▾ " } else { "▸ " },
                    parent.name
                ),
                None => String::new(),
            },
            Row::Sub(p, s) => self
                .parents
                .get(p)
                .and_then(|parent| parent.list.get(s))
                .map(|sub| format!("  {}", sub.name))
                .unwrap_or_default(),
        }
    }

    /// 只给测试用：按键那几条断言要问「光标现在停在哪一行」。
    #[allow(dead_code)]
    pub fn cursor(&self) -> Row {
        self.cursor
    }

    /// 光标在可见行里的下标（画窗口要用）。
    pub fn cursor_index(&self) -> usize {
        self.visible()
            .iter()
            .position(|r| *r == self.cursor)
            .unwrap_or(0)
    }

    pub fn is_expanded(&self, p: usize) -> bool {
        self.expanded.get(p).copied().unwrap_or(false)
    }

    /// 光标那一行现在是展开的还是收起的。
    pub fn cursor_expanded(&self) -> bool {
        match self.cursor {
            Row::All => self.root_expanded,
            Row::Parent(p) => self.is_expanded(p),
            Row::Sub(..) => false,
        }
    }

    /// 在**可见的**行里上下走一格。
    ///
    /// 到底就停住、不绕圈：从最后一个子分区绕回「全部分区」在四百多个分区的列表里
    /// 只会让人怀疑自己按错了（tview 的树也是到底就停）。
    pub fn move_cursor(&mut self, d: i32) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let at = visible.iter().position(|r| *r == self.cursor).unwrap_or(0) as i32;
        let next = (at + d).clamp(0, visible.len() as i32 - 1);
        self.cursor = visible[next as usize];
    }

    /// `←` / `→` 的落点：收起 / 展开光标这一行。
    ///
    /// 返回这个键有没有被树上这一行收下。**叶子不收** —— 叶子上没有可展可收的东西，
    /// 吃掉这个键就等于凭空少一个绑定。Go 版的 `treeKeyCapture` 也是这个形状：
    /// 没什么可做就把事件交回去（不交的话人再也回不到父分区）。
    pub fn toggle(&mut self, expand: bool) -> bool {
        match self.cursor {
            Row::All => {
                self.root_expanded = expand;
                true
            }
            Row::Parent(p) => {
                // 没有子分区的父分区跟叶子一样：没有可展可收的东西
                if self
                    .parents
                    .get(p)
                    .map(|x| x.list.is_empty())
                    .unwrap_or(true)
                {
                    return false;
                }
                if let Some(slot) = self.expanded.get_mut(p) {
                    *slot = expand;
                }
                true
            }
            Row::Sub(..) => false,
        }
    }

    /// 回车停在父分区（或「全部分区」）上：展开 / 收起，跟 `←→` 一个意思。
    pub fn flip(&mut self) -> bool {
        let want = !self.cursor_expanded();
        self.toggle(want)
    }

    /// 回车停在子分区上：**选定了它**。返回要写进配置的那一对
    /// `(area_id, 名字)`，名字带上父分区（「虚拟主播/虚拟日常」），
    /// 跟 Go 版记进配置的形状一致。
    ///
    /// `id <= 0` 一律不算数：0 正是配置里「还没选过」的意思，
    /// 接口哪天把 id 变成别的形状，宁可这一下什么都不做，也不能让用户
    /// 以为选好了、下次启动却又是空的。
    pub fn select(&self) -> Option<(i64, String)> {
        let Row::Sub(p, s) = self.cursor else {
            return None;
        };
        let parent = self.parents.get(p)?;
        let sub = parent.list.get(s)?;
        (sub.id > 0).then(|| (sub.id, format!("{}/{}", parent.name, sub.name)))
    }
}

fn find_sub(parents: &[ParentArea], id: i64) -> Option<(usize, usize)> {
    parents.iter().enumerate().find_map(|(p, parent)| {
        parent
            .list
            .iter()
            .position(|sub| sub.id == id)
            .map(|s| (p, s))
    })
}

/// 从第几行开始画。
///
/// 列表比屏幕长是常态（真接口今天给 12 个父分区、四百多个子分区），窗口得自己算：
/// 不算的话光标一走到底下就看不见了，用户只会得出「按键失灵」这个结论。
///
/// 规则跟大多数列表一样：光标尽量贴着下边走，到了列表尾巴就不再留空档。
pub fn window(total: usize, selected: usize, height: usize) -> usize {
    if height == 0 || total <= height {
        return 0;
    }
    let selected = selected.min(total - 1);
    selected.saturating_sub(height - 1).min(total - height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::area::SubArea;

    /// 三个父分区，第二个里放着「配过的」那个分区 ——
    /// 用两个以上父分区才验得出「只展开配置里那一个」（Go 版踩的就是这里）。
    fn sample() -> Vec<ParentArea> {
        vec![
            ParentArea {
                name: "网游".into(),
                list: vec![
                    SubArea {
                        id: 86,
                        name: "英雄联盟".into(),
                    },
                    SubArea {
                        id: 329,
                        name: "无畏契约".into(),
                    },
                ],
            },
            ParentArea {
                name: "虚拟主播".into(),
                list: vec![SubArea {
                    id: 371,
                    name: "虚拟日常".into(),
                }],
            },
            ParentArea {
                name: "购物".into(),
                list: vec![SubArea {
                    id: 557,
                    name: "好物分享".into(),
                }],
            },
        ]
    }

    #[test]
    fn a_collapsed_tree_shows_one_row_per_parent() {
        let tree = AreaTree::new(sample(), 0);
        assert_eq!(
            tree.visible(),
            vec![Row::All, Row::Parent(0), Row::Parent(1), Row::Parent(2)],
            "收起的父分区只占自己那一行"
        );
        assert_eq!(tree.lines()[0], "▾ 全部分区");
        assert_eq!(tree.lines()[1], "▸ 网游");
    }

    #[test]
    fn expanding_one_parent_splices_its_children_in_place() {
        let mut tree = AreaTree::new(sample(), 0);
        tree.move_cursor(1); // 网游
        assert!(tree.toggle(true), "父分区收下这个键");
        assert_eq!(
            tree.visible(),
            vec![
                Row::All,
                Row::Parent(0),
                Row::Sub(0, 0),
                Row::Sub(0, 1),
                Row::Parent(1),
                Row::Parent(2),
            ]
        );
        assert_eq!(tree.lines()[2], "  英雄联盟");

        // 收回去：光标还在父分区那一行上，子分区整段消失
        assert!(tree.toggle(false));
        assert_eq!(tree.visible().len(), 4);
        assert_eq!(tree.cursor(), Row::Parent(0));
    }

    /// 「全部分区」自己也能收起来：收起来就只剩它一行。
    #[test]
    fn the_root_row_can_be_collapsed() {
        let mut tree = AreaTree::new(sample(), 0);
        assert!(tree.toggle(false));
        assert_eq!(tree.visible(), vec![Row::All]);
        assert_eq!(tree.lines()[0], "▸ 全部分区");
        assert!(tree.toggle(true));
        assert_eq!(tree.visible().len(), 4);
    }

    /// 父分区也是可停的一行：`↑↓` 不许整段跨过去（Go 版就是这个现象）。
    #[test]
    fn parents_are_stoppable_rows() {
        let mut tree = AreaTree::new(sample(), 0);
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Row::Parent(0), "↑↓ 停在父分区上");
        tree.toggle(true);
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Row::Sub(0, 0), "展开后子分区接在父分区后面");
        tree.move_cursor(-1);
        assert_eq!(tree.cursor(), Row::Parent(0), "还能走回来");
        // 收起之后光标停在父分区上，不会指到一个已经看不见的行
        assert!(tree.toggle(false));
        assert_eq!(tree.cursor_index(), 1);
    }

    /// 边界：到顶再按 ↑、到底再按 ↓ 都停在原地（不绕圈、不越界）。
    #[test]
    fn moving_at_the_ends_clamps() {
        let mut tree = AreaTree::new(sample(), 0);
        assert_eq!(tree.cursor(), Row::All);
        for _ in 0..3 {
            tree.move_cursor(-1);
        }
        assert_eq!(tree.cursor(), Row::All, "顶上再往上就是顶上");
        for _ in 0..10 {
            tree.move_cursor(1);
        }
        assert_eq!(tree.cursor(), Row::Parent(2), "到底就停在最后一行");
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Row::Parent(2));
    }

    /// 叶子上 `←→` 什么也不做，而且**不吞键**。
    #[test]
    fn left_right_on_a_leaf_is_not_swallowed() {
        let mut tree = AreaTree::new(sample(), 371); // 光标已经在叶子上
        assert_eq!(tree.cursor(), Row::Sub(1, 0));
        let before = tree.visible();
        assert!(!tree.toggle(true), "叶子上没有可展开的东西：键要交回去");
        assert!(!tree.toggle(false), "收也一样");
        assert!(!tree.flip(), "回车走 flip 也交回去");
        assert_eq!(tree.visible(), before);
        assert_eq!(tree.cursor(), Row::Sub(1, 0));

        // 没有子分区的父分区跟叶子一个待遇：不打标记，也不吃键
        let mut bare = AreaTree::new(
            vec![ParentArea {
                name: "购物".into(),
                list: Vec::new(),
            }],
            0,
        );
        bare.move_cursor(1);
        assert_eq!(bare.lines()[1], "  购物");
        assert!(!bare.toggle(true));
        assert!(!bare.flip());
    }

    /// 没配过：光标在「全部分区」上，一个父分区都不展开。
    #[test]
    fn without_a_saved_area_everything_stays_collapsed() {
        let tree = AreaTree::new(sample(), 0);
        assert_eq!(tree.cursor(), Row::All);
        assert!(
            !tree.visible().iter().any(|r| matches!(r, Row::Sub(..))),
            "没配过就不该有任何子分区露在外面：{:?}",
            tree.visible()
        );
    }

    /// 配过：只展开**配置里那一个**父分区，光标定在那个子分区上，
    /// 别的父分区必须全是收起的。
    #[test]
    fn a_saved_area_expands_only_its_own_parent() {
        let tree = AreaTree::new(sample(), 371); // 371 在「虚拟主播」里，不是第一个父分区
        assert_eq!(tree.cursor(), Row::Sub(1, 0));
        assert!(tree.is_expanded(1), "它所在的那个父分区要展开");
        assert!(
            !tree.is_expanded(0),
            "别的父分区一律收起，不许顺手展开第一个"
        );
        assert!(!tree.is_expanded(2));
        assert_eq!(tree.lines()[1], "▸ 网游");
        assert_eq!(tree.lines()[2], "▾ 虚拟主播");
        assert_eq!(tree.lines()[3], "  虚拟日常");

        // 光标指的那一行就是配过的那个分区
        assert_eq!(tree.select(), Some((371, "虚拟主播/虚拟日常".into())));
        assert_eq!(tree.cursor_index(), 3);
    }

    /// 配过的分区从今天的表里消失了：回到「全部分区 + 全收起」，绝不随便展开一个。
    #[test]
    fn a_saved_area_that_is_gone_falls_back_to_an_all_collapsed_tree() {
        let tree = AreaTree::new(sample(), 999999);
        assert_eq!(tree.cursor(), Row::All);
        assert!((0..3).all(|p| !tree.is_expanded(p)));
    }

    /// 回车停在父分区上时 `select()` 什么也不给（那是展开 / 收起的事）。
    #[test]
    fn select_is_only_for_sub_areas() {
        let mut tree = AreaTree::new(sample(), 0);
        assert_eq!(tree.select(), None, "「全部分区」不是分区");
        tree.move_cursor(1);
        assert_eq!(tree.select(), None, "父分区自己也不是分区");
        tree.toggle(true);
        tree.move_cursor(1);
        assert_eq!(tree.select(), Some((86, "网游/英雄联盟".into())));
    }

    /// 接口没给 id（解析出来是 0）时不认账：0 在配置里是「还没选过」的意思。
    #[test]
    fn select_refuses_a_zero_id() {
        let mut tree = AreaTree::new(
            vec![ParentArea {
                name: "网游".into(),
                list: vec![SubArea {
                    id: 0,
                    name: "英雄联盟".into(),
                }],
            }],
            0,
        );
        tree.move_cursor(1);
        tree.toggle(true);
        tree.move_cursor(1);
        assert_eq!(tree.cursor(), Row::Sub(0, 0));
        assert_eq!(tree.select(), None);
    }

    #[test]
    fn window_keeps_the_cursor_inside_the_visible_rows() {
        const H: usize = 5;
        assert_eq!(window(10, 0, H), 0, "光标在上面时窗口不动");
        assert_eq!(window(10, 4, H), 0, "最后一行正好是窗口底边");
        assert_eq!(window(10, 5, H), 1, "再往下一格窗口跟着走");
        assert_eq!(window(10, 9, H), 5, "到底时窗口贴住列表尾巴");
        assert_eq!(window(3, 9, H), 0, "列表比窗口短：不滚");
        assert_eq!(window(0, 0, H), 0, "空列表");
        assert_eq!(
            window(10, 4, 0),
            0,
            "高度是 0（终端被拉到几乎没有）不许越界"
        );
    }
}
