import { Color, Mesh, MeshStandardMaterial } from "three";
import { useGLTF, Instance, Instances } from "@react-three/drei";
import { type GLTF } from "three-stdlib";
import { useMemo, type JSX } from "react";

type GLTFResult = GLTF & {
  nodes: {
    Mball006: Mesh;
  };
  materials: {};
};

export function CloudSingle(props: JSX.IntrinsicElements["group"]) {
  const { nodes } = useGLTF("/models/cloud.glb") as unknown as GLTFResult;
  return (
    <group {...props} dispose={null}>
      <mesh
        geometry={nodes.Mball006.geometry}
        position={[-0.004, 5, 1.079]}
        scale={0.5}
        rotation={[0, Math.PI, Math.PI / 2]}
        material={useMemo(
          () =>
            new MeshStandardMaterial({
              color: "white",
              emissive: new Color("white"),
              // emissiveIntensity: 0.6,
              wireframe: true,
            }),
          []
        )}
      />
    </group>
  );
}

useGLTF.preload("/cloud.glb");
export function CloudsMulti({
  count = 20,
  planeScale = 50,
  offset = [0, 0, 0],
  height = 10,
}: {
  count?: number;
  planeScale?: number;
  offset?: [number, number, number];
  height?: number;
}) {
  const { nodes } = useGLTF("/models/cloud.glb") as unknown as GLTFResult;

  const clouds = useMemo(() => {
    const result = [];
    for (let i = 0; i < count; i++) {
      const x = (Math.random() - 0.5) * planeScale;
      const z = (Math.random() - 0.5) * planeScale;
      const scale = Math.random() * 0.5 + 1;
      result.push({
        position: [0, height + x, z] as [number, number, number],
        scale,
        key: i,
      });
    }
    return result;
  }, [count, planeScale, offset, height]);

  const material = useMemo(
    () =>
      new MeshStandardMaterial({
        color: "white",
        emissive: new Color("white"),
        emissiveIntensity: 0.6,
        wireframe: true,
      }),
    []
  );

  return (
    <group>
      <Instances
        geometry={nodes.Mball006.geometry}
        material={material}
        position={[offset[0], 5 + offset[1], 1 + offset[2]]}
        scale={0.5}
        rotation={[0, 0, Math.PI / 2]}
      >
        {clouds.map((cloud) => (
          <Instance
            key={cloud.key}
            position={cloud.position}
            scale={(cloud.scale * 3) / 4}
            rotation={[0, Math.random() - 0.5, 0]}
          />
        ))}
      </Instances>
    </group>
  );
}
