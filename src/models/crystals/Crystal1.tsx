import { Mesh } from "three";
import { useGLTF } from "@react-three/drei";
import { type GLTF } from "three-stdlib";
import { type JSX } from "react";

type GLTFResult = GLTF & {
  nodes: {
    Crystal1: Mesh;
  };
  materials: {};
};

export function Crystal1(props: JSX.IntrinsicElements["group"]) {
  const { nodes } = useGLTF("/models/crystals/Crystal1.glb") as unknown as GLTFResult;
  return (
    <group {...props} dispose={null}>
      <mesh geometry={nodes.Crystal1.geometry} scale={0.25}>
        <meshStandardMaterial emissive="#ffffff" emissiveIntensity={0.5} />
      </mesh>
    </group>
  );
}

useGLTF.preload("/models/crystals/Crystal1.glb");
