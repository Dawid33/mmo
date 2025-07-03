
mkdir -p crates/client/assets/parts
# for part in $(ls ldraw/parts); do
#   sed -i 's/\\/\//g' ldraw/parts/$part
# done
# for part in $(ls ldraw/p); do
#   sed -i 's/\\/\//g' ldraw/p/$part
# done
# for part in $(ls ldraw/p/48); do
#   sed -i 's/\\/\//g' ldraw/p/48/$part
# done
# for part in $(ls ldraw/p/8); do
#   sed -i 's/\\/\//g' ldraw/p/8/$part
# done

for part in $(ls ldraw/parts); do
  part=${part//\\//}
  sed -ie 's/\\/\//g' ldraw/parts/$part
  weldr convert gltf $part -C ldraw -o crates/client/assets/parts/$part
done
