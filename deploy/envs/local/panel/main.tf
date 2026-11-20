# The twin's world panel: the live panel module on the imported image mmo/panel:twin (no
# pull secret, no Grafana). `just twin-panel` port-forwards it to http://127.0.0.1:8090/;
# the twin's world (../world) binds it in world-twin.
module "panel" {
  source = "../../../modules/panel"

  image = "mmo/panel:twin"
}
