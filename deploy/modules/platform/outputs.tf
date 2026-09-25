output "game_namespace" {
  value = kubernetes_namespace_v1.game.metadata[0].name
}
