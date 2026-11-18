//! In-memory navigation and page filters for a withdrawn KDE main window.
use super::{settings::SettingsTab, *};

pub(crate) struct TrayWindowState {
    page: Page,
    settings_tab: SettingsTab,
    network_advanced_open: bool,
    page_inputs: [SharedString; 12],
    hide_ips: bool,
    hide_traffic_chart: bool,
    node_sort: super::NodeSort,
    group_by_subscription: bool,
    exact_process_filter: Option<String>,
    connection_sort: xrose_core::flow::ConnectionSort,
    connections_tab: connections::ConnectionsTab,
    content_offset: Point<Pixels>,
    connection_list: ListState,
    connections_limit: usize,
    connections_stats_offset: Point<Pixels>,
    settings_offset: Point<Pixels>,
    log_filter: LogFilter,
    rule_group_id: Option<String>,
    rule_show_all: bool,
    collapsed_subs: std::collections::HashSet<String>,
    mesh_expanded: std::collections::HashSet<String>,
    mesh_card_tabs: std::collections::HashMap<String, mesh::MeshCardTab>,
    collapsed_mesh_sections: std::collections::HashSet<String>,
}

impl AppView {
    // The two windows have different focus and input subscriptions. Carry the text
    // across in memory and let GPUI bind the new controls to their own window.
    fn page_inputs(&self) -> [&Entity<InputState>; 12] {
        [
            &self.node_search,
            &self.connection_search,
            &self.log_search,
            &self.rule_search,
            &self.port_input,
            &self.lan_port_input,
            &self.lan_user_input,
            &self.lan_password_input,
            &self.domestic_urltest_input,
            &self.overseas_urltest_input,
            &self.update_agent_input,
            &self.trace_input,
        ]
    }

    pub(crate) fn tray_state(&self, cx: &App) -> TrayWindowState {
        TrayWindowState {
            page: self.page,
            settings_tab: self.settings_tab,
            network_advanced_open: self.network_advanced_open,
            page_inputs: self.page_inputs().map(|input| input.read(cx).value()),
            content_offset: self.content_scroll.offset(),
            connection_list: self.connection_list.clone(),
            connections_limit: self.connections_limit,
            connections_stats_offset: self.connections_stats_scroll.offset(),
            settings_offset: self.settings_scroll.offset(),
            exact_process_filter: self.exact_process_filter.clone(),
            hide_ips: self.hide_ips,
            hide_traffic_chart: self.hide_traffic_chart,
            node_sort: self.node_sort,
            group_by_subscription: self.group_by_subscription,
            connection_sort: self.connection_sort,
            connections_tab: self.connections_tab,
            log_filter: self.log_filter.clone(),
            rule_group_id: self.rule_group_id.clone(),
            rule_show_all: self.rule_show_all,
            collapsed_subs: self.collapsed_subs.clone(),
            mesh_expanded: self.mesh_expanded.clone(),
            mesh_card_tabs: self.mesh_card_tabs.clone(),
            collapsed_mesh_sections: self.collapsed_mesh_sections.clone(),
        }
    }

    pub(crate) fn restore_from_tray(
        mut self,
        state: TrayWindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        self.page = state.page;
        self.settings_tab = state.settings_tab;
        self.network_advanced_open = state.network_advanced_open;
        self.log_filter = state.log_filter;
        self.rule_group_id = state.rule_group_id;
        self.rule_show_all = state.rule_show_all;
        self.collapsed_subs = state.collapsed_subs;
        self.mesh_expanded = state.mesh_expanded;
        self.mesh_card_tabs = state.mesh_card_tabs;
        self.collapsed_mesh_sections = state.collapsed_mesh_sections;
        self.connection_list = state.connection_list;
        self.connections_limit = state.connections_limit;
        self.connections_stats_scroll
            .set_offset(state.connections_stats_offset);
        self.content_scroll.set_offset(state.content_offset);
        self.settings_scroll.set_offset(state.settings_offset);
        self.exact_process_filter = state.exact_process_filter;
        self.hide_ips = state.hide_ips;
        self.hide_traffic_chart = state.hide_traffic_chart;
        self.node_sort = state.node_sort;
        self.group_by_subscription = state.group_by_subscription;
        self.connection_sort = state.connection_sort;
        self.connections_tab = state.connections_tab;
        for (field, value) in self.page_inputs().into_iter().zip(state.page_inputs) {
            field.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self
    }
}
