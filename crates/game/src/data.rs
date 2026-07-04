use std::collections::BTreeSet;
use std::ops::{BitAndAssign, DerefMut};

use crate::na::{
    Complex, ComplexField, Isometry3, Matrix4, OPoint, Perspective3, Point3, Quaternion, RealField,
    Rotation, Rotation3, Translation3, Unit, Vector3, Vector4,
};
use block_mesh::ndshape::ConstShape;
use na::Translation;
use parry3d::math::RawReal;
use rapier3d::math::Vector;
use rapier3d::prelude::{
    CCDSolver, ColliderSet, DefaultBroadPhase, ImpulseJointSet, IntegrationParameters,
    IslandManager, LockedAxes, MultibodyJointSet, NarrowPhase, QueryPipeline, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet,
};
use rollback::{
    rollback, ClientId, EntityKey, GameDataTransactionKind, GameDataUpdate, GameDataUpdateKind,
    IsometryReal, PlayerKey,
};
// use crate::taffy::style::BlockItemStyle;
// use crate::taffy::TaffyTree;
use crate::{ChunkShape, ClientUpdateEvent};
use borrow::Partial;
use crossbeam::channel::Sender;
use log::info;
use parry3d::math::Real;
use slotmapd::secondary::Iter;
use slotmapd::{new_key_type, Key, KeyData, SecondaryMap, SlotMap, SparseSecondaryMap};
