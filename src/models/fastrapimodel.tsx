import { Mesh, DoubleSide } from "three";
import { useGLTF } from "@react-three/drei";
import { type GLTF } from "three-stdlib";
import { type JSX } from "react";

type GLTFResult = GLTF & {
  nodes: {
    Curve: Mesh;
  };
  materials: {};
};

export function FastrapiModel(props: JSX.IntrinsicElements["group"]) {
  const { nodes } = useGLTF("/models/fastrapi.glb") as unknown as GLTFResult;
  return (
    <group {...props} dispose={null}>
      <mesh
        geometry={nodes.Curve.geometry}
        rotation={[Math.PI / 2, 0, 0]}
        scale={4}
        castShadow
        receiveShadow
      >
        <meshPhysicalMaterial
          color="#e6f7ff"
          roughness={0}
          metalness={0}
          transmission={1}
          transparent
          opacity={0.15}
          ior={1.45}
          thickness={0.5}
          envMapIntensity={1}
          clearcoat={1}
          clearcoatRoughness={0}
          side={DoubleSide}
        />
      </mesh>
    </group>
  );
}

useGLTF.preload("/models/fastrapi.glb");
