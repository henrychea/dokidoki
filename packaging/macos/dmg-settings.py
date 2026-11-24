import os

defines = globals().get("defines", {})
app = defines["app"]
name = os.path.basename(app)

files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines.get("icon")
background = defines["background"]
window_rect = ((0, 0), (660, 400))
default_view = "icon-view"
icon_size = 128
text_size = 13
hide_extensions = [name]
show_status_bar = False
show_tab_view = False
show_pathbar = False
show_sidebar = False
format = "UDZO"
icon_locations = {
    name: (165, 160),
    "Applications": (495, 160),
}
