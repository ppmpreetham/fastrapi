import { useMemo, useRef, type JSX } from "react";
import { useFrame, useThree } from "@react-three/fiber";
import { Instances, Instance, useGLTF } from "@react-three/drei";
import * as THREE from "three";

type Crystal1GLTF = {
  nodes: { Crystal1: THREE.Mesh };
} & THREE.Object3D;

type Crystal2GLTF = {
  nodes: { Crystal2: THREE.Mesh };
} & THREE.Object3D;

const rand = (a: number, b: number) => a + Math.random() * (b - a);

export function CrystalCluster(props: JSX.IntrinsicElements["group"]) {
  const c1 = useGLTF("/models/crystals/Crystal1.glb") as unknown as Crystal1GLTF;
  const c2 = useGLTF("/models/crystals/Crystal2.glb") as unknown as Crystal2GLTF;

  const material = useMemo(
    () =>
      new THREE.MeshStandardMaterial({
        color: "#c9ff61",
        emissive: new THREE.Color("#c9ff61"),
        emissiveIntensity: 0.5,
        wireframe: true,
      }),
    []
  );

  return (
    <group {...props}>
      <mesh
        geometry={c1.nodes.Crystal1.geometry}
        material={material}
        scale={0.1 * rand(0.9, 1.4)}
        rotation={[
          (rand(-1, 1) * Math.PI) / 4,
          Math.random() * 2 * Math.PI,
          (rand(-1, 1) * Math.PI) / 4,
        ]}
      />
      <mesh
        geometry={c2.nodes.Crystal2.geometry}
        material={material}
        scale={0.1 * rand(0.9, 1.4)}
        rotation={[
          (rand(-1, 1) * Math.PI) / 4,
          Math.random() * 2 * Math.PI,
          (rand(-1, 1) * Math.PI) / 4,
        ]}
      />
    </group>
  );
}

export function CrystalField({ count = 50, planeScale = 30, offset = -2 }) {
  return (
    <>
      {Array.from({ length: count }).map((_, i) => (
        <CrystalCluster
          key={i}
          position={[
            (Math.random() - 0.5) * planeScale,
            offset,
            (Math.random() - 0.5) * planeScale,
          ]}
        />
      ))}
    </>
  );
}

export function ProximityCrystals({ count = 2000, radius = 30, proximityRadius = 5 }) {
  const { camera, mouse } = useThree();
  const c1 = useGLTF("/models/crystals/Crystal1.glb") as unknown as Crystal1GLTF;
  const c2 = useGLTF("/models/crystals/Crystal2.glb") as unknown as Crystal2GLTF;
  const useAlt = Math.random() > 0.5;
  const geometry = useAlt ? c1.nodes.Crystal1.geometry : c2.nodes.Crystal2.geometry;

  const material = useMemo(
    () =>
      new THREE.MeshStandardMaterial({
        color: "#c9ff61",
        emissive: "#c9ff61",
        emissiveIntensity: 0.6,
        wireframe: true,
      }),
    []
  );

  const crystalData = useMemo(() => {
    const data = [];
    const gridSize = Math.sqrt(count);
    const spacing = radius / gridSize;

    for (let i = 0; i < count; i++) {
      const x = (Math.random() - 0.5) * radius;
      const z = (Math.random() - 0.5) * radius;

      data.push({
        basePos: new THREE.Vector3(x, -2, z),
        scale: 0,
        rotation: Math.random() * Math.PI * 2,
        rotSpeed: (Math.random() - 0.5) * 0.05,
        baseSize: 0.075 + Math.random() * 0.04,
        offset: Math.random() * Math.PI * 2,
      });
    }
    return data;
  }, [count, radius]);

  const meshRef = useRef<THREE.InstancedMesh>(null!);
  const dummy = useMemo(() => new THREE.Object3D(), []);
  const raycaster = useMemo(() => new THREE.Raycaster(), []);
  const cursorPlane = useMemo(() => new THREE.Plane(new THREE.Vector3(0, 1, 0), 2), []);
  const cursorPos = useMemo(() => new THREE.Vector3(), []);

  useFrame((state) => {
    if (!meshRef.current) return;
    raycaster.setFromCamera(mouse, camera);
    raycaster.ray.intersectPlane(cursorPlane, cursorPos);

    const t = state.clock.elapsedTime;

    for (let i = 0; i < count; i++) {
      const data = crystalData[i];
      const dist = cursorPos.distanceTo(data.basePos);

      const falloff = Math.max(0, 1 - dist / proximityRadius);

      const targetScale = falloff > 0 ? data.baseSize * (1 + falloff * 0.5) : 0;
      data.scale += (targetScale - data.scale) * 0.12;

      if (data.scale > 0.001) {
        dummy.position.copy(data.basePos);
        dummy.rotation.y = data.rotation;

        const scaleVar = 1 + Math.sin(t * 3 + data.offset) * 0.15 * falloff;
        dummy.scale.setScalar(data.scale * scaleVar);

        dummy.updateMatrix();
        meshRef.current.setMatrixAt(i, dummy.matrix);
      } else {
        dummy.position.copy(data.basePos);
        dummy.scale.setScalar(0);
        dummy.updateMatrix();
        meshRef.current.setMatrixAt(i, dummy.matrix);
      }
    }

    meshRef.current.instanceMatrix.needsUpdate = true;

    const maxFalloff = crystalData.reduce((max, data) => {
      const dist = cursorPos.distanceTo(data.basePos);
      const falloff = Math.max(0, 1 - dist / proximityRadius);
      return Math.max(max, falloff);
    }, 0);

    material.emissiveIntensity = 0.5 + maxFalloff * 2.5;
  });

  return <instancedMesh ref={meshRef} args={[geometry, material, count]} frustumCulled={false} />;
}
