import { Merged, useGLTF } from "@react-three/drei";
import { Mesh, MeshStandardMaterial, Color } from "three";
import { useMemo, type JSX } from "react";
import { type GLTF } from "three-stdlib";

type Crystal1GLTF = GLTF & {
  nodes: { Crystal1: Mesh };
};

type Crystal2GLTF = GLTF & {
  nodes: { Crystal2: Mesh };
};

const rand = (a: number, b: number) => a + Math.random() * (b - a);

export function CrystalCluster(props: JSX.IntrinsicElements["group"]) {
  const c1 = useGLTF("/models/crystals/Crystal1.glb") as unknown as Crystal1GLTF;
  const c2 = useGLTF("/models/crystals/Crystal2.glb") as unknown as Crystal2GLTF;
  const material = useMemo(
    () =>
      new MeshStandardMaterial({
        color: "#c9ff61",
        emissive: new Color("#c9ff61"),
        emissiveIntensity: 0.5,
        wireframe: true,
      }),
    []
  );

  return (
    <group {...props}>
      <Merged meshes={{ c1: c1.nodes.Crystal1, c2: c2.nodes.Crystal2 }} material={material}>
        {(merged) => (
          <>
            <merged.c1
              scale={0.1 * rand(0.9, 1.4)}
              rotation={[
                (rand(-1, 1) * Math.PI) / 4,
                Math.random() * 2 * Math.PI,
                (rand(-1, 1) * Math.PI) / 4,
              ]}
            />

            <merged.c2
              scale={0.1 * rand(0.9, 1.4)}
              rotation={[
                (rand(-1, 1) * Math.PI) / 4,
                Math.random() * 2 * Math.PI,
                (rand(-1, 1) * Math.PI) / 4,
              ]}
            />
          </>
        )}
      </Merged>
    </group>
  );
}

export function CrystalField({ count = 50, planeScale = 20, offset = -2 }) {
  return (
    <>
      <mesh rotation={[-Math.PI / 2, 0, 0]} position={[0, offset, 0]}>
        <planeGeometry args={[planeScale, planeScale, 10, 10]} />
        <meshStandardMaterial wireframe />
      </mesh>
      {Array.from({ length: count }).map((_, i) => (
        <CrystalCluster
          key={i}
          position={[
            (Math.random() - 0.5) * planeScale,
            offset,
            (Math.random() - 0.5) * planeScale,
          ]}
          rotation={[0, Math.random() * Math.PI * 2, 0]}
        />
      ))}
    </>
  );
}
