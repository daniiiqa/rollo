# rollo

La vista de workspaces de discada: la pantalla se aleja y quedan los 10
workspaces en miniatura, cada uno con sus ventanas de verdad (capturadas con
el protocolo de Hyprland aunque estén en otro workspace) sobre su wallpaper.

```
rollo        abrir (otra vez: cerrar)
```

Clic en un workspace o una ventana: la miniatura crece hasta llenar la
pantalla y te deja ahí. Arrastrar una ventana a otra miniatura la manda a ese
workspace. Flechas o hjkl y Enter, 1…9 y 0 directo, Esc cierra. Colores,
fuente y bordes salen del look ([disco](https://github.com/daniiiqa/disco)).

Necesita Hyprland (`hyprland_toplevel_export_v1`; el XML del protocolo, de
[hyprland-protocols](https://github.com/hyprwm/hyprland-protocols), va en
`protocols/` con su licencia), `grim` y `awww`.

```
cargo install --git https://github.com/daniiiqa/rollo --root ~/.local --locked
```

MIT.
