# Changelog

## 0.1.1

- Upgrade GPUI Kit and components to 0.8.0 while retaining the default backend.
- Stop unnecessary redraws in the idle sound map and for stationary drags and sliders.
- Stop live metering on compositor-driven map closes and failed window opens.
- Refresh errors and device identity changes correctly while the interface is idle.
- Distinguish DEV builds and isolate their app identity and instance lock;
  release Ctrl+Alt+M and DEV Ctrl+Alt+W remain unchanged.
- Preserve Wayland tray activation, raising, blur and rounded-frame patches.

GPUI Fast remains a separate experiment. This release makes no claim of
measured GPU wattage or laptop battery-life savings.
