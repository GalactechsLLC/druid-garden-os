use crate::plugins::disk_management::{mount, unmount};
use crate::plugins::file_manager::{
    create_directory, create_file, get_file, list_files, remove, rename, update_file,
};
use crate::plugins::system_monitor::{
    get_cpu, get_disks, get_gpus, get_info, get_memory, get_networks,
};
use crate::web::auth::{
    register_endpoint, user_requires_password_update, user_update_password, BasicAuthHandle,
};
use crate::web::config::{config_entry, configs, del_config, upload_config};
use crate::web::farmer::{
    farmer_log_stream, farmer_status, generate_from_mnemonic, get_config, get_farmer_metrics,
    get_farmer_stats, get_farmer_stats_range, is_config_ready, restart_farmer,
    scan_for_legacy_configs, start_farmer, stop_farmer, update_config,
};
use crate::web::leds::{set_color_mode, set_pin_mode};
use crate::web::plugins::{
    add_plugin, all_plugins, available_plugins, del_plugin, del_plugin_environment_value,
    get_plugin_environment, get_plugin_environment_value, plugin, plugin_status, plugin_updates,
    refresh_plugins, set_plugin_environment_value, start_plugin, stop_plugin, update_plugin,
};
use crate::web::system::{
    do_updates, find_device, find_updates, hotspot_active, hotspot_clean, hotspot_restart,
    hotspot_start, hotspot_stop, is_online, wifi_connect, wifi_scan,
};
use portfu::prelude::ServiceGroup;
use portfu_admin::auth::{basic_login, get_jwt};

pub fn none_group(basic_auth: BasicAuthHandle) -> ServiceGroup {
    ServiceGroup::default()
        .shared_state(basic_auth)
        .service(find_device)
        .service(find_updates)
        .service(get_jwt)
        .service(register_endpoint)
        .service(basic_login::<BasicAuthHandle>::default())
}

pub fn user_groups() -> ServiceGroup {
    ServiceGroup::default()
        .service(user_update_password)
        .service(user_requires_password_update)
}

pub fn viewer_group() -> ServiceGroup {
    ServiceGroup::default()
        .service(is_online)
        .service(hotspot_active)
        .service(is_config_ready)
        .service(farmer_status)
        .service(get_farmer_metrics)
        .service(get_farmer_stats)
        .service(get_farmer_stats_range)
        .service(farmer_log_stream {
            peers: Default::default(),
        })
        .service(start_farmer)
        .service(stop_farmer)
        .service(restart_farmer)
        .service(plugin)
        .service(all_plugins)
        .service(available_plugins)
        .service(plugin_updates)
        .service(refresh_plugins)
        .service(plugin_status)
        .service(get_info)
        .service(get_cpu)
        .service(get_gpus)
        .service(get_memory)
        .service(get_disks)
        .service(get_networks)
        .service(scan_for_legacy_configs)
        .service(generate_from_mnemonic)
}

pub fn editor_group() -> ServiceGroup {
    ServiceGroup::default()
        .service(set_pin_mode)
        .service(set_color_mode)
        .service(do_updates)
        .service(wifi_scan)
        .service(wifi_connect)
        .service(hotspot_clean)
        .service(hotspot_stop)
        .service(hotspot_start)
        .service(hotspot_restart)
        .service(get_config)
        .service(update_config)
        .service(get_plugin_environment)
        .service(get_plugin_environment)
        .service(get_plugin_environment)
        .service(get_plugin_environment_value)
        .service(set_plugin_environment_value)
        .service(del_plugin_environment_value)
        .service(start_plugin)
        .service(stop_plugin)
}

pub fn manager_group() -> ServiceGroup {
    ServiceGroup::default()
        .service(add_plugin)
        .service(update_plugin)
        .service(del_plugin)
        .service(config_entry)
        .service(configs)
        .service(upload_config)
        .service(del_config)
        .service(mount)
        .service(unmount)
        .service(list_files)
        .service(get_file)
        .service(create_file)
        .service(update_file)
        .service(create_directory)
        .service(rename)
        .service(remove)
}

pub fn admin_group() -> ServiceGroup {
    ServiceGroup::default()
}

pub fn super_group() -> ServiceGroup {
    ServiceGroup::default()
}
