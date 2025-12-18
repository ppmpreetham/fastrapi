import { useMemo } from "react";
import { InstancedRigidBodies, type InstancedRigidBodyProps } from "@react-three/rapier";
import { useGLTF } from "@react-three/drei";
import { Color, Mesh, MeshStandardMaterial } from "three";
import { type GLTF } from "three-stdlib";

const COUNT = 100;

type FastrapiResult = GLTF & { nodes: { Curve: Mesh }; materials: {} };

export default function RainDown({ planeScale = 40 }: { planeScale?: number }) {
  const { nodes } = useGLTF("/models/fastrapi.glb") as unknown as FastrapiResult;

  const material = useMemo(
    () =>
      new MeshStandardMaterial({
        color: "white",
        emissive: new Color("#c6ff00"),
        emissiveIntensity: 0.6,
      }),
    []
  );

  const instances = useMemo(() => {
    const data: InstancedRigidBodyProps[] = [];
    for (let i = 0; i < COUNT; i++) {
      data.push({
        key: `fastrapi_${i}`,
        position: [
          (Math.random() - 0.5) * planeScale,
          20 + Math.random() * 40,
          (Math.random() - 0.5) * planeScale,
        ],
        rotation: [
          Math.random() * Math.PI * 2,
          Math.random() * Math.PI * 2,
          Math.random() * Math.PI * 2,
        ],
        scale: [2, 2, 2],
      });
    }
    return data;
  }, [planeScale]);

  return (
    <InstancedRigidBodies instances={instances} colliders="hull" restitution={0.5} friction={0.3}>
      <instancedMesh args={[nodes.Curve.geometry, material, COUNT]} count={COUNT} />
    </InstancedRigidBodies>
  );
}

useGLTF.preload("/models/fastrapi.glb");
